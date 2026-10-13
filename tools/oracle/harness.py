"""Shared helpers for the REST oracle scripts: call the official emulator, record each step.

Scripts import this with `sys.path.insert(0, <this directory>)` because `python3 -I` does not
put the script's directory on the path. `send` and `mask` are also what the differential fuzzer
(`tools/fuzz/`) sends requests with and normalises answers with, so its comparisons are the
recordings' own.
"""

import json
import re
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"


def send(host, method, path, body=None, headers=None, raw=None, timeout=30):
    """One HTTP request to `host` (`127.0.0.1:8080`) for `path` (from its `/`): the JSON of
    `body`, or the text `raw` as it is. Answers (status, content type, body as text); raises
    `OSError` (a timeout included) when the server does not answer."""
    if raw is not None:
        data = raw.encode()
    else:
        data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        f"http://{host}{path}",
        method=method,
        data=data,
        headers={"Content-Type": "application/json", **(headers or {})},
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as res:
            return res.status, res.headers.get("Content-Type"), res.read().decode(errors="replace")
    except urllib.error.HTTPError as err:
        return err.code, err.headers.get("Content-Type"), err.read().decode(errors="replace")


def call(method, path, body=None, headers=None):
    status, _, text = send(HOST, method, f"/v1/{path}", body, headers)
    if status < 400:
        return status, json.loads(text) if text else None
    try:
        return status, json.loads(text)
    except ValueError:
        return status, text


def mask(text, project):
    """The recordings' normalisation of a REST answer's body (`rest.py`; `crates/hidane/tests/
    rest.rs` applies the same): the project ID becomes `{project}`, server times (`createTime`,
    `updateTime`, `readTime`, `commitTime`) `<time>`, transaction IDs `<id>`, page tokens
    `<token>` and the IDs generated for documents of a collection `gen` `<auto>`."""
    text = text.replace(project, "{project}")
    text = re.sub(r'"(createTime|updateTime|readTime|commitTime)": "[^"]*"', r'"\1": "<time>"', text)
    text = re.sub(r'"transaction": "[^"]*"', '"transaction": "<id>"', text)
    text = re.sub(r'"nextPageToken": "[^"]*"', '"nextPageToken": "<token>"', text)
    text = re.sub(r'documents/gen/[A-Za-z0-9]{20}"', 'documents/gen/<auto>"', text)
    return text


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


