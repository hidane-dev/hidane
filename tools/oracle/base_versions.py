#!/usr/bin/env python3
"""Record how the official emulator compares an `update_time` precondition with the version of
a document (#262).

Usage: python3 -I tools/oracle/base_versions.py HOST:PORT > fixture.json

A precondition names a version: its time in microseconds, which must be whole and not before
the epoch. The document's version is its update time, the time a delete left, or 0 if it never
existed. Each kind of write (an update, a delete, a transform, a verify) runs with version 0 and
with versions around the stored one and the deleted one; then the times a precondition may not
hold and where that check comes among the others; the Datastore key the official emulator
prints when a version is past the stored one, for names that need escaping, numeric IDs and a
named database; several writes to one document that repeat a precondition; and which `allow`
method Security Rules check for such a write.

A step's times (in its body, or its path) are templates: `{C1}` is the time of the case's first commit, `{C1+1}` a
microsecond later. Versions in the answers are put back the same way (`{V1}`, `{V1+1}`), so a
test can fill both with its own commit times. Each case runs in a project of its own. Stdlib
only.
"""

import json
import re
import sys
import urllib.error
import urllib.request
from datetime import datetime, timedelta, timezone

HOST = sys.argv[1]
cases = []
T1 = "2001-01-01T00:00:00Z"
T0 = "1970-01-01T00:00:00Z"
EPOCH = datetime(1970, 1, 1, tzinfo=timezone.utc)
RULES = """rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /c/{d} {
      allow create: if request.resource.data.k == 'c';
      allow update: if request.resource.data.k == 'u';
      allow read: if true;
    }
  }
}
"""
DENY = """rules_version = '2';
service cloud.firestore {
  match /databases/{database}/documents {
    match /c/{d} {
      allow read, write: if false;
    }
  }
}
"""


def micros(ts):
    t = datetime.strptime(ts, "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=timezone.utc)
    return (t - EPOCH) // timedelta(microseconds=1)


def text(us):
    return (EPOCH + timedelta(microseconds=us)).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def fill(template, commits):
    def time(m):
        return text(commits[int(m.group(1)) - 1] + int(m.group(2) or 0))
    return json.loads(re.sub(r"\{C(\d+)([+-]\d+)?\}", time, json.dumps(template)))


def unfill(message, commits):
    """Puts the case's versions back as templates."""
    def version(m):
        n = int(m.group(0))
        for k, c in enumerate(commits, 1):
            for shift in (0, 1, -1, 3_600_000_000):
                if n == c + shift:
                    return f"{{V{k}{'' if shift == 0 else f'{shift:+d}'}}}"
        return m.group(0)
    return re.sub(r"\b1[6-9][0-9]{14}\b", version, message)


def call(method, path, body, owner):
    headers = {"Content-Type": "application/json"}
    if owner:
        headers["Authorization"] = "Bearer owner"
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status, json.loads(res.read() or b"null")
    except urllib.error.HTTPError as err:
        return err.code, json.loads(err.read() or b"null")


def case(name, steps, database="(default)", rules=None):
    project = f"bv-{len(cases) + 1}"
    db = f"projects/{project}/databases/{database}"
    commits = []
    recorded = []
    if rules is not None:
        status, _ = call("PUT", f"/emulator/v1/projects/{project}:securityRules",
                         {"rules": {"files": [{"name": "firestore.rules", "content": rules}]}}, True)
        assert status == 200, status
    for writes, owner in steps:
        # A commit, or a REST call: (method, path, body).
        method, path, template = ("POST", f"/v1/{db}/documents:commit", {"writes": writes}) \
            if isinstance(writes, list) else writes
        path = path.replace("{base}", f"{db}/documents")
        template = json.loads(json.dumps(template).replace("{base}", f"{db}/documents"))
        status, answer = call(method, fill(path, commits), fill(template, commits), owner)
        step = {"method": method, "path": path, "body": template, "owner": owner, "status": status}
        if status == 200:
            if "commitTime" in answer:
                commits.append(micros(answer["commitTime"]))
        else:
            error = answer["error"]
            step["error"] = {"status": error["status"], "message": unfill(error["message"], commits)}
        recorded.append(step)
    cases.append({"name": name, "rules": rules is not None, "steps": recorded})


def w(kind, doc="c/d", pre=None, fields=None):
    name = "{base}/" + doc
    write = {
        "update": {"update": {"name": name, "fields": fields or {}}},
        "set": {"update": {"name": name, "fields": fields or {}}},
        "delete": {"delete": name},
        "transform": {"transform": {"document": name, "fieldTransforms": [
            {"fieldPath": "t", "increment": {"integerValue": "1"}}]}},
        "verify": {"verify": name},
    }[kind]
    if pre is not None:
        write["currentDocument"] = {"updateTime": pre} if isinstance(pre, str) else pre
    return write


def owner(*writes):
    return (list(writes), True)


# One write: the version a precondition names against the stored one, the deleted one, or 0.
for kind in ("update", "delete", "transform", "verify"):
    case(f"{kind}, version 0, missing document", [owner(w(kind, pre=T0))])
    case(f"{kind}, version 0, existing document", [owner(w("set")), owner(w(kind, pre=T0))])
    case(f"{kind}, version 0, deleted document", [owner(w("set")), owner(w("delete")), owner(w(kind, pre=T0))])
    case(f"{kind}, version 1, missing document", [owner(w(kind, pre="1970-01-01T00:00:00.000001Z"))])
    case(f"{kind}, the stored version", [owner(w("set")), owner(w(kind, pre="{C1}"))])
    case(f"{kind}, a microsecond past the stored version", [owner(w("set")), owner(w(kind, pre="{C1+1}"))])
    case(f"{kind}, an hour past the stored version", [owner(w("set")), owner(w(kind, pre="{C1+3600000000}"))])
    case(f"{kind}, a microsecond before the stored version", [owner(w("set")), owner(w(kind, pre="{C1-1}"))])
    case(f"{kind}, the deleted version", [owner(w("set")), owner(w("delete")), owner(w(kind, pre="{C2}"))])
    case(f"{kind}, a microsecond past the deleted version",
         [owner(w("set")), owner(w("delete")), owner(w(kind, pre="{C2+1}"))])
    case(f"{kind}, a microsecond before the deleted version",
         [owner(w("set")), owner(w("delete")), owner(w(kind, pre="{C2-1}"))])

# The time itself: whole microseconds, not before the epoch, checked with each write's names.
NS = "2001-01-01T00:00:00.000000001Z"
NESTED = {"a": {"arrayValue": {"values": [{"arrayValue": {}}]}}}
for kind in ("update", "delete", "verify"):
    case(f"{kind}, nanoseconds", [owner(w(kind, pre=NS))])
    case(f"{kind}, a microsecond before the epoch", [owner(w(kind, pre="1969-12-31T23:59:59.999999Z"))])
case("update, a tenth of a microsecond", [owner(w("update", pre="2001-01-01T00:00:00.0000001Z"))])
case("update, year 1", [owner(w("update", pre="0001-01-01T00:00:00Z"))])
case("update, year 9999", [owner(w("update", pre="9999-12-31T23:59:59.999999Z"))])
case("update, before the epoch with nanoseconds", [owner(w("update", pre="1969-12-31T23:59:59.000000001Z"))])
case("nanoseconds, then a name that does not parse", [owner(w("update", pre=NS), w("update", doc="c"))])
case("a name that does not parse, then nanoseconds", [owner(w("update", doc="c"), w("update", pre=NS))])
case("nanoseconds, then a mask that does not parse",
     [owner(w("update", pre=NS), {**w("update", doc="c/e"), "updateMask": {"fieldPaths": ["a..b"]}})])
case("a mask that does not parse, then nanoseconds",
     [owner({**w("update", doc="c/e"), "updateMask": {"fieldPaths": ["a..b"]}}, w("update", pre=NS))])
case("a mask that does not parse and nanoseconds", [owner({**w("update", pre=NS), "updateMask": {"fieldPaths": ["a..b"]}})])
case("a reserved field name and nanoseconds", [owner(w("update", pre=NS, fields={"__x__": {"nullValue": None}}))])
case("a nested array and nanoseconds", [owner(w("update", pre=NS, fields=NESTED))])
case("a nested array, then nanoseconds", [owner(w("update", doc="c/e", fields=NESTED), w("update", pre=NS))])
case("a refused sequence, then nanoseconds",
     [owner(w("delete", doc="c/e"), w("update", doc="c/e", pre={"exists": True}), w("update", pre=NS))])
case("a delete with a mask and nanoseconds", [owner({**w("delete", pre=NS), "updateMask": {"fieldPaths": ["a"]}})])

# The key printed for a version past the stored one.
for doc in ["c/é", 'c/a"b', "c/a'b", "c/a\\b", "c/a\tb", "c/a\nb", "c/a\rb", "c/a\x0bb", "c/a\x08b", "c/a\x0cb",
            "c/a\x07b", "c/a\x01b", "c/a\x7fb", "c/\x80", "c/ÿ", "c/Ā", "c/€", "c/😀", "é/d", "a/b/c/d/e/f",
            "c/__id5__", "c/__id-5__", "c/__id1999999999__", "c/__id2000000000__", "c/__id-1999999999__",
            "c/__id-2000000000__", "c/__id9223372036854775807__", "c/__id-9223372036854775808__", "c/__id1__x"]:
    case(f"the key of {doc!r}", [owner(w("set", doc=doc)), owner(w("update", doc=doc, pre="{C1+1}"))])
case("the key in a named database", [owner(w("set")), owner(w("update", pre="{C1+1}"))], database="other")

# Several writes to one document repeat the first one's precondition, which is checked once,
# against the document as it was before the commit.
case("delete, set, update at the stored version",
     [owner(w("set")), owner(w("delete", pre="{C1}"), w("set"), w("update", pre="{C1}"))])
case("update, set, update at the stored version",
     [owner(w("set")), owner(w("update", pre="{C1}", fields={"v": {"integerValue": "1"}}), w("set"), w("update", pre="{C1}"))])
case("update twice at the stored version",
     [owner(w("set")), owner(w("update", pre="{C1}", fields={"v": {"integerValue": "1"}}), w("update", pre="{C1}"))])
case("delete, set, delete at the stored version",
     [owner(w("set")), owner(w("delete", pre="{C1}"), w("set"), w("delete", pre="{C1}"))])
case("delete then update at version 0, missing document", [owner(w("delete", pre=T0), w("update", pre=T0))])
case("update then delete at version 0, missing document", [owner(w("update", pre=T0), w("delete", pre=T0))])
case("delete then update at version 0, existing document", [owner(w("set")), owner(w("delete", pre=T0), w("update", pre=T0))])
case("past the stored version, then a document that is missing",
     [owner(w("set")), owner(w("update", pre="{C1+3600000000}"), w("update", doc="c/e", pre={"exists": True}))])
case("a document that is missing, then past the stored version",
     [owner(w("set")), owner(w("update", doc="c/e", pre={"exists": True}), w("update", pre="{C1+3600000000}"))])
case("past the stored version, then a set", [owner(w("set")), owner(w("update", pre="{C1+3600000000}"), w("set"))])
case("the deleted version after a write to another document",
     [owner(w("set")), owner(w("delete")), owner(w("set", doc="c/e"), w("update", pre="{C2}"))])
case("update at version 2001 then at version 0", [owner(w("update", pre=T1), w("update", pre=T0))])

# Security Rules: version 0 is a create, any other version an update, and the rules come first.
K = lambda k: {"k": {"stringValue": k}}
for k in ("c", "u"):
    case(f"rules, version 0, missing document, k={k}", [([w("update", pre=T0, fields=K(k))], False)], rules=RULES)
    case(f"rules, version 0, existing document, k={k}",
         [owner(w("set", fields=K("x"))), ([w("update", pre=T0, fields=K(k))], False)], rules=RULES)
    case(f"rules, version 2001, missing document, k={k}", [([w("update", pre=T1, fields=K(k))], False)], rules=RULES)
    case(f"rules, the stored version, k={k}",
         [owner(w("set", fields=K("x"))), ([w("update", pre="{C1}", fields=K(k))], False)], rules=RULES)
case("rules, past the stored version", [owner(w("set", fields=K("x"))), ([w("update", pre="{C1+1}", fields=K("u"))], False)],
     rules=RULES)
case("rules, the deleted version",
     [owner(w("set", fields=K("x"))), owner(w("delete")), ([w("update", pre="{C2}", fields=K("u"))], False)], rules=RULES)
case("rules that deny, past the stored version",
     [owner(w("set")), ([w("update", pre="{C1+1}")], False)], rules=DENY)
case("rules that deny, the deleted version",
     [owner(w("set")), owner(w("delete")), ([w("update", pre="{C2}")], False)], rules=DENY)

# REST reads its `currentDocument.updateTime` parameter as version 0 on the official emulator,
# as it reads `readTime` as the epoch (docs/parity-exceptions.md); hidane reads the time.
case("REST patch, currentDocument.updateTime at the stored version",
     [owner(w("set")), (("PATCH", "/v1/{base}/c/d?currentDocument.updateTime={C1}", {"fields": {}}), True)])

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/base_versions.py",
    "cases": cases,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
