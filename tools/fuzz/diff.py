"""Runs a fuzz case on both emulators and compares their answers.

`run` sends a case's steps to one emulator, in a fresh project, through the oracle's transport
(`tools/oracle/harness.py`'s `send`). `compare` normalises both runs' answers with the oracle's
normalisation (`harness.mask`) plus the values only a fuzz case makes run-dependent, finds the
first step whose answers differ and classifies that difference:

- `crash`: one side answered 5xx, or did not answer, and the other did not do the same;
- `status`: different HTTP status codes, or the same with a different error status;
- `message`: the same error, worded differently;
- `order`: the same values in another order (list elements, or the fields of an object);
- `data`: anything else (values, layout, content type).

Each difference gets a signature that does not depend on the seed: the same problem found by
other inputs gets the same one, so issues and reports deduplicate on it. `Exceptions` drops the
differences `docs/parity-exceptions.md` accepts, by the rules in `exceptions.json`, each tied
to an entry of `docs/parity-exceptions.json`; a rule can look at the request too, since one
signature (an official 200 against hidane's `Payload isn't valid for request.`) can come from
an accepted difference and from a bug. Stdlib only.
"""

import hashlib
import json
import re
import sys
import time
import uuid
from pathlib import Path
from urllib.parse import parse_qsl, quote

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "oracle"))
from harness import mask, send  # noqa: E402

KINDS = ("crash", "status", "message", "order", "data")
VARIABLES = ("tx", "token", "commitTime", "updateTime", "cursor", "version")
# Datastore mode: entity versions (commit times in microseconds) and cursors, which are opaque
# bytes each emulator makes its own way (docs/adr/0009-datastore-mode.md), as the recordings mask
# them (tools/oracle/datastore_entities.py).
DATASTORE_VERSION = re.compile(r'"(version|commitVersion|snapshotVersion)": "[0-9]+"')
DATASTORE_CURSOR = re.compile(r'"(cursor|endCursor|skippedCursor|startCursor)": "[^"]*"')


def project():
    """A fresh project ID, as long as the recordings' `rest-HHMMSS` (document sizes count it)."""
    return "f" + uuid.uuid4().hex[:3] + "-" + uuid.uuid4().hex[:6]


def fill(text, values, encode):
    for key, value in values.items():
        text = text.replace("{" + key + "}", quote(value, safe="") if encode and key != "project" else value)
    return text


def fill_json(value, values):
    if isinstance(value, str):
        return fill(value, values, False)
    if isinstance(value, list):
        return [fill_json(v, values) for v in value]
    if isinstance(value, dict):
        return {k: fill_json(v, values) for k, v in value.items()}
    return value


def capture(text, values):
    """Keeps the run-dependent values an answer holds, for the steps that name them."""
    try:
        answer = json.loads(text)
    except ValueError:
        return
    first = answer[0] if isinstance(answer, list) and answer and isinstance(answer[0], dict) else answer
    if not isinstance(first, dict):
        return
    if isinstance(first.get("transaction"), str):
        values["tx"] = first["transaction"]
    if isinstance(first.get("nextPageToken"), str):
        values["token"] = first["nextPageToken"]
    batch = first.get("batch")
    if isinstance(batch, dict) and isinstance(batch.get("endCursor"), str):
        values["cursor"] = batch["endCursor"]
    mutations = first.get("mutationResults")
    if isinstance(mutations, list) and mutations and isinstance(mutations[0], dict) \
            and isinstance(mutations[0].get("version"), str):
        values["version"] = mutations[0]["version"]
    if isinstance(first.get("commitTime"), str):
        values["commitTime"] = first["commitTime"]
    results = first.get("writeResults")
    if isinstance(results, list) and results and isinstance(results[0], dict) and "updateTime" in results[0]:
        values["updateTime"] = results[0]["updateTime"]
    elif isinstance(first.get("updateTime"), str):
        values["updateTime"] = first["updateTime"]


# `REQUEST_TIME` has millisecond precision on both emulators: two commits in one millisecond
# write the same time, so the second changes nothing and moves no field (#329). The official
# emulator never answers that fast; a step that asks for it waits this long after the last.
REQUEST_TIME_GAP = 0.002


def run(host, case, timeout=10.0, sender=send):
    """Sends the case's steps to `host` in a fresh project; each answer, normalised:
    `{"status", "contentType", "body"}`, or `{"status": None, "error": ...}` for no answer."""
    values = {"project": project(), "commitTime": "1970-01-01T00:00:00Z", "updateTime": "1970-01-01T00:00:00Z"}
    answers = []
    answered = None
    for step in case["steps"]:
        if answered is not None and "REQUEST_TIME" in json.dumps(step.get("body")) + step.get("raw", ""):
            time.sleep(max(0.0, REQUEST_TIME_GAP - (time.monotonic() - answered)))
        path = fill(step["path"], values, True)
        headers = {k: fill(v, values, False) for k, v in step.get("headers", {}).items()}
        raw = fill(step["raw"], values, False) if "raw" in step else None
        body = fill_json(step.get("body"), values)
        try:
            status, content_type, text = sender(host, step["method"], path, body, headers, raw, timeout)
        except (OSError, ValueError) as err:
            answers.append({"status": None, "error": type(err).__name__})
            continue
        finally:
            answered = time.monotonic()
        capture(text, values)
        answers.append({"status": status, "contentType": content_type, "body": normalize(text, values)})
    return answers


def normalize(text, values):
    """The oracle's normalisation, then the values the case's run chose: the transaction IDs,
    page tokens, cursors, versions and times it captured, times of the years the fuzzer never
    writes (2021 to 2099: the server's), versions of those years in microseconds (precondition
    messages), and Datastore's entity versions and cursors."""
    text = mask(text, values["project"])
    text = DATASTORE_VERSION.sub(r'"\1": "<version>"', text)
    text = DATASTORE_CURSOR.sub(r'"\1": "<cursor>"', text)
    for key in VARIABLES:
        value = values.get(key)
        if value and len(value) > 3 and not value.startswith("1970-"):
            text = text.replace(value, "<" + key + ">")
    text = re.sub(r"20(2[1-9]|[3-9][0-9])-[0-9]{2}-[0-9]{2}T[0-9:.]+Z", "<now>", text)
    return re.sub(r"(version \(|exactBaseVersion=)(1[6-9]|[23][0-9]|40|41)[0-9]{14}(?![0-9])", r"\1<version>", text)


class Num:
    """A JSON number as written: `1` and `1.0` differ."""
    __slots__ = ("text",)

    def __init__(self, text):
        self.text = text

    def __eq__(self, other):
        return isinstance(other, Num) and other.text == self.text

    def __hash__(self):
        return hash(self.text)

    def __repr__(self):
        return self.text


def parse(text):
    try:
        return json.loads(text, parse_float=Num, parse_int=Num, parse_constant=Num)
    except (ValueError, TypeError):
        return None


def canonical(value):
    """A value's text with object keys and list elements sorted."""
    if isinstance(value, dict):
        return "{" + ",".join(json.dumps(k) + ":" + canonical(v) for k, v in sorted(value.items())) + "}"
    if isinstance(value, list):
        return "[" + ",".join(sorted(canonical(v) for v in value)) + "]"
    if isinstance(value, Num):
        return "#" + value.text
    return json.dumps(value)


def generalize(path):
    """A JSON path without what varies between inputs: list indexes and field names."""
    out = []
    for i, part in enumerate(path):
        if isinstance(part, int):
            out.append("[]")
        elif i > 0 and path[i - 1] in ("fields", "properties"):
            out.append("*")
        else:
            out.append(part)
    return ".".join(out)


def first_difference(a, b, path=()):
    """The path of the first place two parsed values differ, and whether only order differs there."""
    if canonical(a) == canonical(b):
        if a == b and list_order(a) == list_order(b):
            return None
        # Only order differs: name the deepest place that holds it.
        if isinstance(a, dict) and list(a) == list(b):
            return next(filter(None, (first_difference(a[k], b[k], path + (k,)) for k in a)), (path, True))
        if isinstance(a, list) and all(canonical(x) == canonical(y) for x, y in zip(a, b)):
            return next(filter(None, (first_difference(x, y, path + (n,)) for n, (x, y) in enumerate(zip(a, b)))),
                        (path, True))
        return path, True
    if isinstance(a, dict) and isinstance(b, dict):
        if a.keys() != b.keys():
            return path + (sorted(a.keys() ^ b.keys())[0],), False
        for key in a:
            found = first_difference(a[key], b[key], path + (key,))
            if found:
                return found
    if isinstance(a, list) and isinstance(b, list) and len(a) == len(b):
        for n, (x, y) in enumerate(zip(a, b)):
            found = first_difference(x, y, path + (n,))
            if found:
                return found
    return path, False


def list_order(value):
    """Object key order, which `==` on dicts does not see."""
    if isinstance(value, dict):
        return [(k, list_order(v)) for k, v in value.items()]
    if isinstance(value, list):
        return [list_order(v) for v in value]
    return None


def error_of(parsed):
    if isinstance(parsed, list) and len(parsed) == 1:
        parsed = parsed[0]
    if isinstance(parsed, dict) and isinstance(parsed.get("error"), dict):
        e = parsed["error"]
        return {"status": e.get("status") if isinstance(e.get("status"), str) else None,
                "message": e.get("message") if isinstance(e.get("message"), str) else None}
    return None


def template(message):
    """An error message without the values a request put in it. A Security Rules denial becomes
    the kinds of reasons it lists, in order of first appearance."""
    if message is None:
        return ""
    if reasons(message) or message.strip() == "No matching allow statements":
        kinds = []
        for part in [message.strip()] if not reasons(message) else re.split(r", (?=\S)", message.strip()):
            part = re.sub(r" for '[a-z]+' @ L[0-9]+$", "", part)
            part = re.sub(r"evaluation error at L[0-9]+:[0-9]+", "evaluation error", part)
            part = re.sub(r"Property \S+ is undefined", "Property … is undefined", part)
            if part not in kinds:
                kinds.append(part)
        return ("\\n" if message.startswith("\n") else "") + "; ".join(kinds)[:160]
    message = re.sub(r"projects/[^\s\"',]*", "<name>", message)
    message = re.sub(r"EntityRef\[[^\]]*\]", "EntityRef[…]", message)
    message = re.sub(r'"[^"]*"', '"…"', message)
    message = re.sub(r"'[^']*'", "'…'", message)
    message = re.sub(r"`[^`]*`", "`…`", message)
    message = re.sub(r"\[[^\]]*\]", "[…]", message)
    message = re.sub(r"[0-9]+", "N", message)
    return message[:160]


def verb(step):
    path = step["path"].split("?")[0]
    if re.match(r"/v1/projects/[^/]*:", path):
        # The Datastore API: `datastore:commit` is not Firestore's `:commit`.
        return "datastore" + path[path.index(":"):]
    if path.startswith("/emulator/"):
        return step["method"] + " emulator" + (":" + path.rsplit(":", 1)[1] if ":" in path.rsplit("/", 1)[-1] else "")
    tail = path.rsplit("/", 1)[-1]
    if ":" in tail:
        return tail[tail.index(":"):]
    after = path.split("/documents", 1)[1].strip("/") if "/documents" in path else ""
    parts = [p for p in after.split("/") if p] if after else []
    return step["method"] + (" document" if parts and len(parts) % 2 == 0 else " collection" if parts else " root")


def classify(step, official, hidane):
    """The difference between two answers to one step, or None when they are the same."""
    if official.get("status") == hidane.get("status") and official.get("contentType") == hidane.get("contentType") \
            and official.get("body") == hidane.get("body") and official.get("error") == hidane.get("error"):
        return None
    o_status, h_status = official.get("status"), hidane.get("status")
    o_parsed, h_parsed = parse(official.get("body") or ""), parse(hidane.get("body") or "")
    o_error, h_error = error_of(o_parsed), error_of(h_parsed)
    detail, values = "", (None, None)
    if o_status != h_status and (o_status is None or h_status is None or o_status >= 500 or h_status >= 500):
        kind = "crash"
        sides = [side for side, status in (("official", o_status), ("hidane", h_status))
                 if status is None or status >= 500]
        detail = "+".join(sides) + ":" + "/".join(str(s) for s in (o_status, h_status))
    elif o_status != h_status or (o_error and h_error and o_error["status"] != h_error["status"]):
        kind = "status"
        # The errors' templates too: refusals of different things (an untyped value, a nested
        # transform) are different differences, even of one request with the same statuses.
        detail = f"{(o_error or {}).get('status')}/{(h_error or {}).get('status')}"
        templates = [template(e["message"]) for e in (o_error, h_error) if e]
        if templates:
            detail += ": " + " | ".join(templates)
    elif o_error and h_error:
        kind = "message"
        detail = template(o_error["message"]) + " | " + template(h_error["message"])
    elif o_parsed is not None and h_parsed is not None and official.get("contentType") == hidane.get("contentType"):
        found = first_difference(o_parsed, h_parsed)
        if found is None:
            kind, detail = "data", "layout"
        elif found[1] and "<auto>" in repr(at(o_parsed, found[0])):
            # Generated document IDs are masked: where one comes among others is chance.
            return None
        else:
            kind = "order" if found[1] else "data"
            detail = generalize(found[0])
            if not found[1]:
                values = (leaf(o_parsed, found[0]), leaf(h_parsed, found[0]))
    else:
        kind, detail = "data", "content type" if official.get("contentType") != hidane.get("contentType") else "body"
    out = {"kind": kind, "verb": verb(step), "detail": detail,
           "official": summary(official, o_error), "hidane": summary(hidane, h_error)}
    for side, value in zip(("official", "hidane"), values):
        if value is not None:
            out[side]["value"] = value
    return out


def at(parsed, path):
    """The value at `path`, or None."""
    for part in path:
        if isinstance(parsed, dict) and part in parsed:
            parsed = parsed[part]
        elif isinstance(parsed, list) and isinstance(part, int) and part < len(parsed):
            parsed = parsed[part]
        else:
            return None
    return parsed


def leaf(parsed, path):
    """The text of the string or number at `path`, if there is one there."""
    for part in path:
        if isinstance(parsed, dict) and part in parsed:
            parsed = parsed[part]
        elif isinstance(parsed, list) and isinstance(part, int) and part < len(parsed):
            parsed = parsed[part]
        else:
            return None
    if isinstance(parsed, (str, Num)):
        return str(parsed)[:300]
    return None


def summary(answer, error):
    out = {"status": answer.get("status")}
    if answer.get("error"):
        out["error"] = answer["error"]
    if error:
        out["code"] = error["status"]
        out["message"] = error["message"]
    return out


def signature(difference):
    """Stable across seeds and areas: what differs, in which request, and how, without the
    inputs' values (`data-batchget-0123456789ab`)."""
    parts = [difference["kind"], difference["verb"], str(difference["official"]["status"]),
             str(difference["hidane"]["status"]), difference["detail"]]
    digest = hashlib.sha256("\x1f".join(parts).encode()).hexdigest()[:12]
    slug = re.sub(r"[^a-z0-9]+", "-", difference["verb"].lower()).strip("-")
    return f"{difference['kind']}-{slug}-{digest}"


def compare(case, official, hidane):
    """The first step whose answers differ, classified, or None."""
    for n, step in enumerate(case["steps"]):
        o = official[n] if n < len(official) else {"status": None, "error": "missing"}
        h = hidane[n] if n < len(hidane) else {"status": None, "error": "missing"}
        found = classify(step, o, h)
        if found:
            found["step"] = n
            found["signature"] = signature(found)
            return found
    return None


def reasons(message):
    """The reasons a Security Rules denial lists (`false for 'get' @ L5`, …)."""
    return re.findall(r"(?:^|, )(.*?for '[a-z]+' @ L[0-9]+)(?=, |$)", (message or "").strip())


def hidane_reasons_subset(official, hidane):
    """hidane's denial lists some of the official one's reasons and nothing else: the official
    emulator's two passes (D008) or its `false` for what was allowed (D018, D019) add the rest.
    Where its first pass failed on `resource` or a lookup it says `evaluation error at L6:23 for
    'create' @ L6`, which stands for any reason of that statement, as in the tests'
    `same_denial`: hidane evaluates once, with what that pass lacked. Where it failed on another
    error, the second pass's `false` may be missing (a batch the first pass decided on a later
    target, an upsert's create statement): the error stands for hidane's `false` there."""
    mine = reasons(hidane.get("message"))
    theirs = reasons(official.get("message"))
    statement = lambda r: r[r.rindex(" for '"):]
    failed_first = {statement(r) for r in theirs if r.startswith("evaluation error at L")}
    errors = {statement(r) for r in theirs if not r.startswith("false for '")}
    return bool(mine) and all(r in theirs or statement(r) in failed_first
                              or (r.startswith("false for '") and statement(r) in errors) for r in mine)


RELATIONS = {"hidane-reasons-subset": hidane_reasons_subset}


def body_of(step):
    """A step's body as JSON values: `body`, or `raw` when it parses (a raw body can hold escapes
    such as `\\ud800` that only exist once parsed), else None."""
    if "body" in step:
        return step["body"]
    try:
        return json.loads(step["raw"]) if "raw" in step else None
    except ValueError:
        return None


def strings(value, keys=True):
    """Every string in a JSON value, its object keys too unless `keys` is false."""
    if isinstance(value, str):
        yield value
    elif isinstance(value, list):
        for item in value:
            yield from strings(item, keys)
    elif isinstance(value, dict):
        for key, item in value.items():
            if keys:
                yield key
            yield from strings(item, keys)


def query_values(step):
    """The values of the query parameters of a step's path, decoded."""
    _, _, query = step["path"].partition("?")
    return [value for _, value in parse_qsl(query, keep_blank_values=True)]


def lone_surrogate(step):
    """A string of the body, a key or a value, holds an unpaired surrogate (`\\ud800`). JSON can
    escape one; once parsed, a pair is one character, so what is left in U+D800 to U+DFFF is
    unpaired."""
    return any(any(0xD800 <= ord(c) <= 0xDFFF for c in s) for s in strings(body_of(step)))


LOOSE_TIME = re.compile(r"(\d+)-(\d+)-(\d+)T(\d+):(\d+):(\d+)(\.\d*)?(Z|[+-](\d+):(\d+))")
STRICT_TIME = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})")


def strict_time(text):
    """Whether `text` is an RFC 3339 timestamp: four digits for the year and two for each other
    field, each in range (a second of 60 is a leap second), an offset of at most 23:59."""
    match = LOOSE_TIME.fullmatch(text)
    if not match or not STRICT_TIME.fullmatch(text):
        return False
    year, month, day, hour, minute, second = (int(match.group(n)) for n in range(1, 7))
    leap = year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
    days = [31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    offset = (0, 0) if match.group(8) == "Z" else (int(match.group(9)), int(match.group(10)))
    return (1 <= month <= 12 and 1 <= day <= days[month - 1] and hour <= 23 and minute <= 59
            and second <= 60 and offset[0] <= 23 and offset[1] <= 59)


def lenient_timestamp(step):
    """A string value of the body, or a query parameter's value, is shaped like a timestamp
    (digits, `-`, `T`, `:`, an optional fraction, `Z` or an offset) but is not RFC 3339: a field
    out of range (month 13, hour 24, an offset of 24 hours) or of another width (`2020-1-01`,
    `00.Z`)."""
    texts = list(strings(body_of(step), keys=False)) + query_values(step)
    return any(LOOSE_TIME.fullmatch(t) and not strict_time(t) for t in texts)


# The length of the fuzzer's project IDs (`project()`), which replace `{project}` in a path.
PROJECT_LENGTH = len("rest-000000")


def long_request_line(step):
    """The request line (`METHOD /path?query HTTP/1.1`) is longer than 4096 bytes, each
    `{project}` of the path counted as a project ID of the fuzzer's. Paths are ASCII
    (percent-encoded)."""
    path = step["path"].replace("{project}", "p" * PROJECT_LENGTH)
    return len(f"{step['method']} {path} HTTP/1.1") > 4096


def several_targets(step):
    """A commit or batch write of more than one write, or a batchGet of more than one document."""
    body = body_of(step)
    return isinstance(body, dict) and any(isinstance(body.get(key), list) and len(body[key]) > 1
                                          for key in ("writes", "documents"))


def in_transaction(step):
    """The body begins a read-write transaction (`newTransaction: {readWrite: …}`) or names one
    (`transaction`; a read-only one locks nothing, so no lock follows from it)."""
    body = body_of(step)
    if not isinstance(body, dict):
        return False
    new = body.get("newTransaction")
    return (isinstance(new, dict) and "readWrite" in new) or isinstance(body.get("transaction"), str)


def entity_operand(step):
    """An aggregation whose filter orders a property against an entity value (`<`, `<=`, `>`,
    `>=`, `=`, `IN`)."""
    body = body_of(step)
    if not isinstance(body, dict) or not isinstance(body.get("aggregationQuery"), dict):
        return False

    def walk(f):
        if not isinstance(f, dict):
            return False
        composite = f.get("compositeFilter")
        if isinstance(composite, dict):
            return any(walk(c) for c in composite.get("filters") or [])
        prop = f.get("propertyFilter")
        if not isinstance(prop, dict) or prop.get("op") == "NOT_EQUAL":
            return False
        value = prop.get("value")
        values = value.get("arrayValue", {}).get("values") or [] if isinstance(value, dict) and "arrayValue" in value else [value]
        return any(isinstance(v, dict) and "entityValue" in v for v in values)

    nested = body["aggregationQuery"].get("nestedQuery")
    return isinstance(nested, dict) and walk(nested.get("filter"))


def other_body_project(step):
    """A Datastore API request (`/v1/projects/{project}:verb`) whose body names a `projectId`
    other than the path's."""
    body = body_of(step)
    return (re.fullmatch(r"/v1/projects/\{project\}:[A-Za-z]+", step["path"]) is not None
            and isinstance(body, dict) and isinstance(body.get("projectId"), str)
            and body["projectId"] not in ("", "{project}"))


def collection_group(step):
    """The body's query reads a collection group or every descendant (`allDescendants`), in a
    `structuredQuery` or a `structuredAggregationQuery`."""
    body = body_of(step)
    if not isinstance(body, dict):
        return False
    query = body.get("structuredQuery")
    aggregation = body.get("structuredAggregationQuery")
    if query is None and isinstance(aggregation, dict):
        query = aggregation.get("structuredQuery")
    selectors = query.get("from") if isinstance(query, dict) else None
    return isinstance(selectors, list) and any(isinstance(s, dict) and s.get("allDescendants") is True for s in selectors)


# Named predicates on a request (a step of a case: its method, path, headers and body), which a
# rule's `request` and `earlier` list under `predicates`.
PREDICATES = {
    "lone-surrogate": lone_surrogate,
    "lenient-timestamp": lenient_timestamp,
    "long-request-line": long_request_line,
    "collection-group": collection_group,
    "in-transaction": in_transaction,
    "several-targets": several_targets,
    "other-body-project": other_body_project,
    "entity-operand": entity_operand,
}


class Exceptions:
    """The differences docs/parity-exceptions.md accepts, as rules (`exceptions.json`).

    A rule names the entry of `docs/parity-exceptions.json` that accepts the difference
    (`exception`, its ID) and says which differences it covers: `kinds`, `areas`, `verbs` (a
    regular expression on the step's verb), `detail` (one on where the answers differ), for
    `official` and `hidane` the `status` (a number or a list of them, `null` for no answer), the
    error `code` and `message`, and the `value` where a `data` difference is (regular
    expressions), and a `relation` between the two answers (`RELATIONS`).

    `request` says what the request of the step that differs is: its `method` and `path`
    (regular expressions; the path as the case gives it, with `{project}` and the other
    placeholders), `headers` (a regular expression per header, names in any case; a header the
    step does not set is the empty string) and `predicates` (names from `PREDICATES`, each of
    which must hold). `earlier` says the same of some step before that one, and the `status` of
    its answer (the same on both sides: the answers differ only later). Everything a rule gives
    must match; a rule with `request` or `earlier` never matches without the case."""

    KEYS = {"id", "exception", "why", "kinds", "areas", "verbs", "detail", "official", "hidane", "relation",
            "request", "earlier"}
    SIDE_KEYS = {"status", "code", "message", "value"}
    REQUEST_KEYS = {"method", "path", "headers", "predicates"}

    def __init__(self, rules):
        self.rules = rules

    @classmethod
    def load(cls, path=Path(__file__).with_name("exceptions.json")):
        return cls(json.loads(Path(path).read_text())["rules"])

    def check(self, entries):
        """Problems with the rules, given the IDs of docs/parity-exceptions.json's entries."""
        problems = []
        seen = set()
        for rule in self.rules:
            rid = rule.get("id")
            if not isinstance(rid, str) or not re.fullmatch(r"[a-z0-9-]+", rid):
                problems.append(f"rule {rid!r}: `id` must be lowercase words joined by hyphens")
            if rid in seen:
                problems.append(f"rule {rid}: repeated")
            seen.add(rid)
            if set(rule) - self.KEYS:
                problems.append(f"rule {rid}: unknown keys {sorted(set(rule) - self.KEYS)}")
            if rule.get("exception") not in entries:
                problems.append(f"rule {rid}: `exception` {rule.get('exception')!r} is not an entry of docs/parity-exceptions.json")
            if not rule.get("why"):
                problems.append(f"rule {rid}: say `why` the entry covers these differences")
            for kind in rule.get("kinds", []):
                if kind not in KINDS:
                    problems.append(f"rule {rid}: unknown kind {kind!r}")
            for side in ("official", "hidane"):
                spec = rule.get(side, {})
                if set(spec) - self.SIDE_KEYS:
                    problems.append(f"rule {rid}: unknown keys in `{side}`: {sorted(set(spec) - self.SIDE_KEYS)}")
                for key in ("code", "message", "value"):
                    if key in spec:
                        problems += self.regex(rid, f"{side}.{key}", spec[key])
            for key in ("verbs", "detail"):
                if key in rule:
                    problems += self.regex(rid, key, rule[key])
            for key in ("request", "earlier"):
                if key in rule:
                    problems += self.check_request(rid, key, rule[key])
            if "relation" in rule and rule["relation"] not in RELATIONS:
                problems.append(f"rule {rid}: unknown relation {rule['relation']!r}")
            if not any(k in rule for k in ("official", "hidane", "verbs")):
                problems.append(f"rule {rid}: too broad; give `official`, `hidane` or `verbs`")
        return problems

    @staticmethod
    def regex(rid, where, pattern):
        try:
            re.compile(pattern)
        except (re.error, TypeError) as err:
            return [f"rule {rid}: `{where}` is not a regular expression ({err})"]
        return []

    def check_request(self, rid, key, spec):
        if not isinstance(spec, dict):
            return [f"rule {rid}: `{key}` must be an object"]
        allowed = self.REQUEST_KEYS | ({"status"} if key == "earlier" else set())
        problems = []
        if set(spec) - allowed:
            problems.append(f"rule {rid}: unknown keys in `{key}`: {sorted(set(spec) - allowed)}")
        for field in ("method", "path"):
            if field in spec:
                problems += self.regex(rid, f"{key}.{field}", spec[field])
        headers = spec.get("headers", {})
        if not isinstance(headers, dict):
            problems.append(f"rule {rid}: `{key}.headers` must map header names to regular expressions")
        else:
            for name, pattern in headers.items():
                problems += self.regex(rid, f"{key}.headers.{name}", pattern)
        predicates = spec.get("predicates", [])
        if not isinstance(predicates, list):
            problems.append(f"rule {rid}: `{key}.predicates` must be a list of names")
        else:
            for name in predicates:
                if name not in PREDICATES:
                    problems.append(f"rule {rid}: unknown predicate {name!r} in `{key}`")
        return problems

    def match(self, area, difference, case=None, official=None):
        """The first rule that accepts the difference, or None. `case` is the case whose step
        `difference["step"]` differs and `official` the official emulator's answers to its
        steps, for the rules that look at the requests."""
        for rule in self.rules:
            if "kinds" in rule and difference["kind"] not in rule["kinds"]:
                continue
            if "areas" in rule and area not in rule["areas"]:
                continue
            if "verbs" in rule and not re.search(rule["verbs"], difference["verb"]):
                continue
            if "detail" in rule and not re.search(rule["detail"], difference["detail"]):
                continue
            if "relation" in rule and not RELATIONS[rule["relation"]](difference["official"], difference["hidane"]):
                continue
            if not all(self.side(rule.get(side, {}), difference[side]) for side in ("official", "hidane")):
                continue
            if ("request" in rule or "earlier" in rule) and not self.requests(rule, difference, case, official):
                continue
            return rule
        return None

    @staticmethod
    def side(spec, answer):
        if "status" in spec:
            wanted = spec["status"] if isinstance(spec["status"], list) else [spec["status"]]
            if answer.get("status") not in wanted:
                return False
        for key in ("code", "message", "value"):
            if key in spec and not re.search(spec[key], answer.get(key) or ""):
                return False
        return True

    @classmethod
    def requests(cls, rule, difference, case, official):
        """Whether the case's requests are those `rule` gives (`request`, `earlier`)."""
        steps = (case or {}).get("steps", [])
        at = difference.get("step")
        if not isinstance(at, int) or at >= len(steps):
            return False
        if "request" in rule and not cls.request(rule["request"], steps[at]):
            return False
        if "earlier" in rule:
            spec, answers = rule["earlier"], official or []
            wanted = spec.get("status")
            if wanted is not None and not isinstance(wanted, list):
                wanted = [wanted]
            return any(cls.request(spec, steps[n])
                       and (wanted is None or (n < len(answers) and answers[n].get("status") in wanted))
                       for n in range(at))
        return True

    @staticmethod
    def request(spec, step):
        if "method" in spec and not re.search(spec["method"], step["method"]):
            return False
        if "path" in spec and not re.search(spec["path"], step["path"]):
            return False
        headers = {name.lower(): value for name, value in step.get("headers", {}).items()}
        for name, pattern in spec.get("headers", {}).items():
            if not re.search(pattern, headers.get(name.lower(), "")):
                return False
        return all(PREDICATES[name](step) for name in spec.get("predicates", []))


def parity_entries(path=ROOT / "docs" / "parity-exceptions.json"):
    return {entry["id"] for entry in json.loads(Path(path).read_text())["differences"]}
