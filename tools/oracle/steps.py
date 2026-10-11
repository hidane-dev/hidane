"""Records REST calls as replayable steps, for the Security Rules oracles (#63–#68).

A script opens a case, makes its calls through `call`, and dumps `cases`: each case is a name
and its steps, each step the request (method, path, body, `Authorization`) and the official
emulator's answer (status, and the body: JSON when it parses). hidane's tests replay the steps
in order and compare the answers. Stdlib only.
"""

import json
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
OWNER = "Bearer owner"
cases = []


def case(name, **extra):
    cases.append({"name": name, **extra, "steps": []})


def call(method, path, body=None, auth=None, record=True):
    headers = {"Content-Type": "application/json"}
    if auth:
        headers["Authorization"] = auth
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            status, text = res.status, res.read().decode()
    except urllib.error.HTTPError as err:
        status, text = err.code, err.read().decode()
    try:
        response = json.loads(text) if text else None
    except ValueError:
        response = text
    if record and cases:
        cases[-1]["steps"].append({"method": method, "path": path, "body": body, "auth": auth,
                                   "status": status, "response": response})
    return status, text


def rules(project, content):
    status, text = call("PUT", f"/emulator/v1/projects/{project}:securityRules",
                        {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    assert status == 200, text


def dump():
    json.dump(cases, sys.stdout, indent=1, ensure_ascii=False)
    print()
