#!/usr/bin/env python3
"""Record how the official emulator answers document reads and writes (#16, #85).

Usage: python3 -I tools/oracle/document_writes.py HOST:PORT > fixture.json

Each case is a list of REST calls against a fresh project; every call's HTTP status and body
are recorded. gRPC returns the same status code and message, so hidane's gRPC tests compare
against these. Timestamps are kept; tests compare them relative to each other, never verbatim.
Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]


def call(method, path, body=None, headers=None):
    req = urllib.request.Request(
        f"http://{HOST}/v1/{path}",
        method=method,
        data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json", **(headers or {})},
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            raw = res.read()
            return res.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            return err.code, json.loads(raw)
        except ValueError:
            return err.code, raw.decode(errors="replace")


cases = []


def shift(ts, micros):
    """Moves an RFC 3339 UTC timestamp with microseconds by `micros`."""
    from datetime import datetime, timedelta, timezone
    t = datetime.strptime(ts, "%Y-%m-%dT%H:%M:%S.%fZ").replace(tzinfo=timezone.utc)
    return (t + timedelta(microseconds=micros)).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def case(name, project, steps):
    """Placeholders: {base}, {db}, {token} (last nextPageToken), {commitTime},
    {commitTime-1us} and {commitTime+1h} (from the last response with a commitTime)."""
    from urllib.parse import quote
    base = f"projects/{project}/databases/(default)/documents"
    db = f"projects/{project}/databases/(default)"
    vars = {}
    recorded = []
    for method, path, body, *rest in steps:
        headers = rest[0] if rest else None

        def fill(text, encode):
            text = text.replace("{base}", base).replace("{db}", db)
            for k, v in vars.items():
                text = text.replace("{" + k + "}", quote(v, safe="") if encode else v)
            return text

        full = fill(path, True)
        if isinstance(body, dict):
            body = json.loads(fill(json.dumps(body), False))
        status, response = call(method, full, body, headers)
        recorded.append({"method": method, "path": full, "body": body, "status": status, "response": response})
        if isinstance(response, dict):
            if "nextPageToken" in response:
                vars["token"] = response["nextPageToken"]
            if "commitTime" in response:
                vars["commitTime"] = response["commitTime"]
                vars["commitTime-1us"] = shift(response["commitTime"], -1)
                vars["commitTime+1h"] = shift(response["commitTime"], 3_600_000_000)
    cases.append({"name": name, "steps": recorded})


def update(name, fields, mask=None, exists=None, update_time=None):
    w = {"update": {"name": "{base}/" + name, "fields": fields}}
    if mask is not None:
        w["updateMask"] = {"fieldPaths": mask}
    if exists is not None:
        w["currentDocument"] = {"exists": exists}
    if update_time is not None:
        w["currentDocument"] = {"updateTime": update_time}
    return w


def delete(name, exists=None):
    w = {"delete": "{base}/" + name}
    if exists is not None:
        w["currentDocument"] = {"exists": exists}
    return w


def commit(*writes):
    return ("POST", "{base}:commit", {"writes": list(writes)})


i = lambda n: {"integerValue": str(n)}
s = lambda v: {"stringValue": v}
m = lambda **f: {"mapValue": {"fields": f}}

case("get missing document", "p-get-missing", [("GET", "{base}/c/d", None)])
case("create twice", "p-create-twice", [
    ("POST", "{base}/c?documentId=d", {"fields": {"a": i(1)}}),
    ("POST", "{base}/c?documentId=d", {"fields": {"a": i(2)}}),
])
case("create without id", "p-create-auto", [("POST", "{base}/c", {"fields": {"a": i(1)}})])
case("update exists=true on missing", "p-upd-missing", [commit(update("c/d", {"a": i(1)}, exists=True))])
case("update exists=false on existing", "p-upd-existing", [
    commit(update("c/d", {"a": i(1)})),
    commit(update("c/d", {"a": i(2)}, exists=False)),
])
case("update_time precondition", "p-upd-time", [
    commit(update("c/d", {"a": i(1)})),
    commit(update("c/d", {"a": i(2)}, update_time="2001-01-01T00:00:00Z")),
])
case("delete exists=true on missing", "p-del-missing", [commit(delete("c/d", exists=True))])
case("delete missing without precondition", "p-del-plain", [commit(delete("c/d"))])
case("identical set twice", "p-identical", [
    commit(update("c/d", {"a": i(1)})),
    commit(update("c/d", {"a": i(1)})),
    ("GET", "{base}/c/d", None),
])
case("update mask", "p-mask", [
    commit(update("c/d", {"a": i(1), "b": i(2), "c": m(x=i(1), y=i(2))})),
    commit(update("c/d", {"a": i(10), "c": m(x=i(5)), "e": i(9)}, mask=["a", "c.x", "d"])),
    ("GET", "{base}/c/d", None),
])
case("update mask on missing document", "p-mask-missing", [
    commit(update("c/d", {"a": i(1), "b": m(x=i(1))}, mask=["b.x"])),
    ("GET", "{base}/c/d", None),
])
case("two writes to one document in a commit", "p-twice", [commit(update("c/d", {"a": i(1)}), update("c/d", {"a": i(2)}))])
case("empty commit", "p-empty", [("POST", "{base}:commit", {})])
case("odd document name", "p-odd", [("GET", "{base}/c", None)])
case("reserved document id", "p-reserved-id", [commit(update("c/__x__", {"a": i(1)}))])
case("dot document id", "p-dot-id", [commit(update("c/.", {"a": i(1)}))])
case("reserved field name", "p-reserved-field", [commit(update("c/d", {"__x__": i(1)}))])
case("nested array", "p-nested", [commit(update("c/d", {"a": {"arrayValue": {"values": [{"arrayValue": {}}]}}}))])
case("invalid mask path", "p-bad-mask", [commit(update("c/d", {"a": i(1)}, mask=["a..b"]))])
case("batch get order", "p-batchget", [
    commit(update("c/a", {"n": i(1)}), update("c/b", {"n": i(2)})),
    ("POST", "{base}:batchGet", {"documents": ["{base}/c/b", "{base}/c/missing", "{base}/c/a"]}),
])
case("get with mask", "p-get-mask", [
    commit(update("c/d", {"a": i(1), "b": m(x=i(1), y=i(2)), "c": i(3)})),
    ("GET", "{base}/c/d?mask.fieldPaths=b.x&mask.fieldPaths=c", None),
])
case("get with read_time before creation", "p-readtime", [
    commit(update("c/d", {"a": i(1)})),
    ("GET", "{base}/c/d?readTime=2001-01-01T00:00:00Z", None),
    ("GET", "{base}/c/d?readTime=2999-01-01T00:00:00Z", None),
])
case("list documents", "p-list", [
    commit(update("c/a", {"n": i(1)}), update("c/b", {"n": i(2)}), update("c/c", {"n": i(3)}),
           update("c/ghost/sub/x", {"n": i(4)})),
    ("GET", "{base}/c", None),
    ("GET", "{base}/c?pageSize=2", None),
    ("GET", "{base}/c?showMissing=true", None),
])
case("list documents page 2", "p-list2", [
    commit(update("c/a", {"n": i(1)}), update("c/b", {"n": i(2)}), update("c/c", {"n": i(3)})),
    ("GET", "{base}/c?pageSize=2", None),
])
case("list collection ids", "p-ids", [
    commit(update("users/a", {"n": i(1)}), update("users/a/posts/p", {"n": i(1)}), update("ghost/g/x/y", {"n": i(1)})),
    ("POST", "{base}:listCollectionIds", {}),
    ("POST", "{base}:listCollectionIds", {}, {"Authorization": "Bearer owner"}),
    ("POST", "{base}/users/a:listCollectionIds", {}, {"Authorization": "Bearer owner"}),
    ("POST", "{base}:listCollectionIds", {"pageSize": 1}, {"Authorization": "Bearer owner"}),
])
case("batch write", "p-batchwrite", [
    commit(update("c/exists", {"n": i(1)})),
    ("POST", "{base}:batchWrite", {"writes": [
        update("c/new", {"n": i(1)}),
        update("c/missing", {"n": i(1)}, exists=True),
        update("c/exists", {"n": i(2)}, exists=False),
        delete("c/exists"),
    ]}),
])
case("named database", "p-named", [
    ("GET", "projects/p-named/databases/second/documents/c/d", None),
    ("POST", "projects/p-named/databases/second/documents:commit", {"writes": [{"update": {"name": "projects/p-named/databases/second/documents/c/d", "fields": {"a": i(1)}}}]}),
])

case("read_time around a commit", "p-readtime2", [
    commit(update("c/d", {"a": i(1)})),
    ("GET", "{base}/c/d?readTime={commitTime}", None),
    ("GET", "{base}/c/d?readTime={commitTime-1us}", None),
    ("GET", "{base}/c/d?readTime={commitTime+1h}", None),
])
case("numeric id document", "p-numeric", [
    commit(update("c/__id5__", {"a": i(1)})),
    ("GET", "{base}/c/__id5__", None),
    commit(update("c/__id5x__", {"a": i(1)})),
])
case("batch write statuses", "p-batchwrite2", [
    commit(update("c/exists", {"n": i(1)})),
    ("POST", "{base}:batchWrite", {"writes": [
        update("c/new", {"n": i(1)}),
        update("c/missing", {"n": i(1)}, exists=True),
        update("c/exists", {"n": i(2)}, exists=False),
        delete("c/other"),
    ]}),
    ("GET", "{base}/c/new", None),
])
case("list documents with missing, as admin", "p-list-admin", [
    commit(update("c/a", {"n": i(1)}), update("c/ghost/sub/x", {"n": i(4)})),
    ("GET", "{base}/c?showMissing=true", None, {"Authorization": "Bearer owner"}),
])
case("list documents next page", "p-list3", [
    commit(update("c/a", {"n": i(1)}), update("c/b", {"n": i(2)}), update("c/c", {"n": i(3)})),
    ("GET", "{base}/c?pageSize=2", None),
    ("GET", "{base}/c?pageSize=2&pageToken={token}", None),
])
case("update_time precondition that matches", "p-upd-time-ok", [
    commit(update("c/d", {"a": i(1)})),
    commit(update("c/d", {"a": i(2)}, update_time="{commitTime}")),
])

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/document_writes.py",
    "cases": cases,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
