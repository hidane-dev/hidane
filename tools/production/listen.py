"""Replays the recording of what a listener's query target holds against production Cloud
Firestore, over WebChannel (#82).

`listen_queries.json` (`tools/oracle/listen_queries.py`) holds the dataset and the queries of
`queries.json`. Its recorder added each query as the target of a Listen stream of its own over
WebChannel, the browser SDK's transport, and recorded the documents the target sent before it
became `CURRENT`, in the order sent, or why it was removed. Here the dataset is written and each
target added to a Listen stream of production's WebChannel, as the recorder opened them: the
token goes in the first request's `headers`, as the SDK sends it, and in an `Authorization`
header. `LISTENS_AT_ONCE` streams are open at a time. A target production removes because it
needs an index the emulators never ask for (D070) is counted apart, and the index kept in the
report (`indexes.py`). Stdlib only.
"""

import http.client
import json
import re
import time
import urllib.parse
from concurrent.futures import ThreadPoolExecutor

from replay import (INDEX_LINK, NEEDS_INDEX, NETWORK_TRIES, PROJECT_NAME, asked, first_difference, log, mask, same_json,
                    seed, tally)

CHANNEL = "/google.firestore.v1.Firestore/Listen/channel"
# Streams open at a time, and how long a target may take to become current.
LISTENS_AT_ONCE = 8
CURRENT_WITHIN = 30
# A stream's requests: the first, with the target; the stream itself; and its end.
REQUESTS_PER_LISTEN = 3


# Differences of production's Listen the parity exceptions record: (entry, what the official
# emulator's target did and its message, what production's did and its message); None matches
# any. Production takes cursors and filters the official emulator refuses (D073, D074, D107), and
# checks a target on its own, with messages of its own (D117).
KNOWN_LISTEN = [
    ("D073", "removed", r"^Cursor has too many values\.$", "documents", None),
    ("D074", "removed", r"^Firestore does not support descending key scans$", "documents", None),
    ("D107", "removed", r"^The query may not have more than 100 filters", "documents", None),
    ("D107", None, None, "removed", r"^Too many disjunctions after normalization"),
    ("D106", None, None, "removed", r"^A maximum of 1 'ARRAY_CONTAINS' filter is allowed per disjunction\.$"),
    ("D117", None, None, "removed", None),
]


def known_listen(official, answered):
    """The entry of `KNOWN_LISTEN` that two targets' outcomes show, or None."""
    for entry, theirs, their_message, ours, our_message in KNOWN_LISTEN:
        if ((theirs is None or theirs in official)
                and (their_message is None or re.search(their_message, str(official.get(theirs, ""))))
                and ours in answered and (our_message is None or re.search(our_message, str(answered[ours])))):
            return entry
    return None


class Frames:
    """The `<length>\\n<JSON>` frames of a WebChannel response, as they arrive (the lengths count
    UTF-16 units)."""

    def __init__(self, response):
        self.response, self.buffer = response, ""

    def next(self, deadline):
        while True:
            newline = self.buffer.find("\n")
            if newline >= 0:
                length = int(self.buffer[:newline])
                text, units, i = self.buffer[newline + 1:], 0, 0
                while i < len(text) and units < length:
                    units += 2 if ord(text[i]) > 0xFFFF else 1
                    i += 1
                if units == length:
                    self.buffer = text[i:]
                    return json.loads(text[:i])
            if time.monotonic() > deadline:
                return None
            data = self.response.read1(65536)
            if not data:
                return None
            self.buffer += data.decode("utf-8")


def connection(base):
    parts = urllib.parse.urlsplit(base)
    kind = http.client.HTTPSConnection if parts.scheme == "https" else http.client.HTTPConnection
    return kind(parts.netloc, timeout=CURRENT_WITHIN + 30)


def listen(production, target):
    """What a Listen stream's target holds when it becomes current: the names of the documents, in
    the order sent, below the documents; or the message of its removal, or of an error."""
    production.spend(REQUESTS_PER_LISTEN)
    database = f"projects/{production.project}/databases/(default)"
    documents = f"{database}/documents"
    authorization = {"Authorization": f"Bearer {production.token}"}

    def query(params):
        return urllib.parse.urlencode(params, safe="()")

    body = {"headers": f"Authorization:Bearer {production.token}\r\n", "count": 1, "ofs": 0,
            "req0___data__": json.dumps({"database": database, "addTarget": {**target, "targetId": 2}})}
    c = connection(production.base)
    c.request("POST", CHANNEL + "?" + query({"VER": 8, "database": database, "RID": 1000, "CVER": 22,
                                             "X-HTTP-Session-Id": "gsessionid", "zx": "x", "t": 1}),
              body=urllib.parse.urlencode(body),
              headers={**authorization, "Content-Type": "application/x-www-form-urlencoded"})
    r = c.getresponse()
    if r.status != 200:
        return {"error": f"{r.status} {mask(r.read().decode(errors='replace'), production.project)[:300]}"}
    session = r.getheader("X-HTTP-Session-Id")
    sid = Frames(r).next(time.monotonic() + CURRENT_WITHIN)[0][1][1]
    ours = {"database": database, **({"gsessionid": session} if session else {}), "SID": sid}
    c = connection(production.base)
    c.request("GET", CHANNEL + "?" + query({"VER": 8, **ours, "RID": "rpc", "AID": 0, "CI": 0, "TYPE": "xmlhttp",
                                            "zx": "x", "t": 1}), headers=authorization)
    frames = Frames(c.getresponse())
    found, outcome, deadline = [], None, time.monotonic() + CURRENT_WITHIN
    while outcome is None:
        frame = frames.next(deadline)
        if frame is None:
            outcome = {"timeout": True}
            break
        for _, messages in frame:
            for m in messages if isinstance(messages, list) else [messages]:
                if not isinstance(m, dict):
                    continue
                if "documentChange" in m:
                    found.append(m["documentChange"]["document"]["name"][len(documents) + 1:])
                change = m.get("targetChange", {})
                if change.get("targetChangeType") == "CURRENT":
                    outcome = {"documents": found}
                elif change.get("targetChangeType") == "REMOVE":
                    outcome = {"removed": mask(change.get("cause", {}).get("message", ""), production.project)}
                if "error" in m:
                    outcome = {"error": json.loads(mask(json.dumps(m["error"]), production.project))}
    c = connection(production.base)
    c.request("POST", CHANNEL + "?" + query({"VER": 8, **ours, "RID": 1001, "TYPE": "terminate", "zx": "x"}),
              headers={**authorization, "Content-Type": "text/plain;charset=UTF-8"})
    c.getresponse().read()
    return outcome


def listened(production, target):
    """`listen`, tried again a few times when the connection fails before an answer; then the
    outcome says so."""
    for attempt in range(NETWORK_TRIES):
        try:
            return listen(production, target)
        except (OSError, http.client.HTTPException, ValueError, IndexError) as err:
            log(f"Listen: {type(err).__name__}, try {attempt + 1} of {NETWORK_TRIES}")
            time.sleep(2 ** attempt)
    return {"error": "no answer"}


def replay_listen(production, recording, result):
    """`listen_queries.json`: the dataset is written, then each query is the target of a Listen
    stream of its own (`listen`), and what it held when current, or why it was removed, is
    compared with what the official emulator's did. A difference `KNOWN_LISTEN` records counts as
    known."""
    fill = seed(production, recording["dataset"])
    documents = f"projects/{production.project}/databases/(default)/documents"
    cases, targets = [], []
    for case in recording["cases"]:
        if any(name != "{project}" for name in PROJECT_NAME.findall(json.dumps(case["query"]))):
            result["skipped"]["names another project"] = result["skipped"].get("names another project", 0) + 1
            continue
        parent = documents + ("/" + case["parent"] if case["parent"] else "")
        cases.append(case)
        targets.append({"query": {"parent": parent, "structuredQuery": fill(case["query"])}})
    log(f"{result['fixture']}: {len(targets)} targets, {LISTENS_AT_ONCE} at a time")
    with ThreadPoolExecutor(LISTENS_AT_ONCE) as pool:
        outcomes = list(pool.map(lambda target: listened(production, target), targets))
    for case, answered in zip(cases, outcomes):
        result["sent"] += 1
        removed = answered.get("removed", "")
        asked(result, removed)
        if NEEDS_INDEX.search(removed) and "documents" in case["outcome"]:
            result["needsIndex"].append(case["name"])
            continue
        answered = json.loads(INDEX_LINK.sub("<index link>", json.dumps(answered)))
        about = None if same_json(case["outcome"], answered, set()) else known_listen(case["outcome"], answered)
        if about:
            result["knownCases"] += 1
            result["known"][about] = result["known"].get(about, 0) + 1
            continue
        tally(result, case["outcome"], answered, lambda: {
            "name": case["name"], "method": "Listen", "path": CHANNEL,
            "difference": first_difference(case["outcome"], answered) or "the outcome",
            "official": case["outcome"], "production": answered}, case["name"])
