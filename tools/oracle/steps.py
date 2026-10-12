"""Records REST calls as replayable steps, for the Security Rules oracles (#63–#68).

A script opens a case, makes its calls through `call`, and dumps `cases`: each case is a name
and its steps, each step the request (method, path, body, `Authorization`) and the official
emulator's answer (status, and the body: JSON when it parses). hidane's tests replay the steps
in order and compare the answers. Stdlib only.

With `raw=True` a step also keeps the body as sent (`raw`) and its content type, and a step
sent with another `content_type` (or `None`, for none) keeps it. With `LOG` set to the path of
the official emulator's standard output, a step keeps what the emulator printed while it ran
(`log`), such as the values of `debug()`.
"""

import json
import os
import sys
import time
import urllib.error
import urllib.request

HOST = sys.argv[1]
OWNER = "Bearer owner"
cases = []
# Whether to record the body of a successful answer (the tests compare only statuses then).
KEEP_OK = True
# The official emulator's standard output, to record what each call printed.
LOG = None
JSON = "application/json"


def case(name, **extra):
    cases.append({"name": name, **extra, "steps": []})


def _printed(since):
    """What the emulator printed after `since` bytes of its output, once it has settled."""
    time.sleep(0.15)
    with open(LOG, "rb") as f:
        f.seek(since)
        return f.read().decode()


def call(method, path, body=None, auth=None, record=True, raw=False, content_type=JSON):
    headers = {}
    if content_type is not None:
        headers["Content-Type"] = content_type
    if auth:
        headers["Authorization"] = auth
    since = os.path.getsize(LOG) if LOG else 0
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            status, text, answered = res.status, res.read().decode(), res.headers.get("Content-Type")
    except urllib.error.HTTPError as err:
        status, text, answered = err.code, err.read().decode(), err.headers.get("Content-Type")
    try:
        response = json.loads(text) if text else None
    except ValueError:
        response = text
    if record and cases:
        kept = response if KEEP_OK or status >= 300 else None
        step = {"method": method, "path": path, "body": body, "auth": auth, "status": status, "response": kept}
        if content_type != JSON:
            step["content_type"] = content_type
        if raw:
            step["raw"] = text
            step["answered"] = answered
        if LOG:
            printed = _printed(since)
            if printed:
                step["log"] = printed
        cases[-1]["steps"].append(step)
    return status, text


def rules(project, content):
    status, text = call("PUT", f"/emulator/v1/projects/{project}:securityRules",
                        {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200, text


def dump():
    """Writes `cases` as JSON, one step per line."""
    compact = lambda v: json.dumps(v, ensure_ascii=False, separators=(",", ":"))
    print("[")
    for i, c in enumerate(cases):
        head = ",".join(f"{compact(k)}:{compact(v)}" for k, v in c.items() if k != "steps")
        steps = ",\n".join("  " + compact(step) for step in c["steps"])
        print(f'{{{head},"steps":[\n{steps}\n]}}' + ("," if i + 1 < len(cases) else ""))
    print("]")
