#!/usr/bin/env python3
"""What does `request.auth` hold for each kind of token? (#68)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_auth.py 127.0.0.1:8086 > fixture.json

rules-unit-testing and the SDKs' `mockUserToken` send unsigned (`alg: none`) JWTs. For each
token (claims present or missing, expired, for another project, `Bearer owner`, none) and each
probe condition, the script loads `allow read: if <probe>;`, reads a document over REST with
that `Authorization` header and records the status. Stdlib only.
"""

import base64
import json
import sys
import time
import urllib.error
import urllib.request

HOST = sys.argv[1]
PROJECT = "auth-rules"
BASE = f"projects/{PROJECT}/databases/(default)/documents"
NOW = int(time.time())


def b64(obj):
    return base64.urlsafe_b64encode(json.dumps(obj, separators=(",", ":")).encode()).decode().rstrip("=")


def token(claims):
    return "Bearer " + b64({"alg": "none", "kid": "fakekid", "typ": "JWT"}) + "." + b64(claims) + "."


FULL = {"iss": f"https://securetoken.google.com/{PROJECT}", "aud": PROJECT, "iat": NOW, "exp": NOW + 3600,
        "auth_time": NOW, "sub": "u1", "user_id": "u1", "email": "a@example.com", "email_verified": True,
        "firebase": {"sign_in_provider": "password", "identities": {}}, "admin": True}


def without(*keys):
    return {k: v for k, v in FULL.items() if k not in keys}


TOKENS = {
    "none": None,
    "owner": "Bearer owner",
    "full": token(FULL),
    "sub only": token({"sub": "u1"}),
    "user_id only": token({"user_id": "u1"}),
    "no sub or user_id": token(without("sub", "user_id")),
    "sub and user_id differ": token({**FULL, "user_id": "u2"}),
    "expired": token({**FULL, "exp": NOW - 3600}),
    "other project": token({**FULL, "aud": "other", "iss": "https://securetoken.google.com/other"}),
    "no iss or aud": token(without("iss", "aud")),
    "not a jwt": "Bearer something",
}

PROBES = [
    "true", "request.auth == null", "request.auth != null", "request.auth.uid == 'u1'", "request.auth.uid == 'u2'",
    "request.auth.token.sub == 'u1'", "request.auth.token.user_id == 'u1'", "request.auth.token.email == 'a@example.com'",
    "request.auth.token.email_verified == true", "request.auth.token.firebase.sign_in_provider == 'password'",
    "request.auth.token.admin == true", "request.auth.token.aud == 'auth-rules'", "request.auth.token.exp is int",
    "request.auth.token.iat is timestamp", "request.auth.token.exp is timestamp", "request.auth.token.keys().size() > 3",
]


def call(method, path, body=None, auth=None):
    headers = {"Content-Type": "application/json"}
    if auth:
        headers["Authorization"] = auth
    req = urllib.request.Request(f"http://{HOST}{path}", method=method,
                                 data=None if body is None else json.dumps(body).encode(), headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=30) as res:
            return res.status, res.read().decode()
    except urllib.error.HTTPError as err:
        return err.code, err.read().decode()


call("POST", f"/v1/{BASE}/c?documentId=d", {"fields": {"n": {"integerValue": "1"}}}, auth="Bearer owner")
results = []
for probe in PROBES:
    content = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
               f"    match /c/{{d}} {{\n      allow read: if {probe};\n    }}\n  }}\n}}\n")
    status, body = call("PUT", f"/emulator/v1/projects/{PROJECT}:securityRules",
                        {"rules": {"files": [{"name": "firestore.rules", "content": content}]}})
    entry = {"condition": probe, "reads": {}}
    if status != 200:
        entry["compile"] = body
    for name, auth in TOKENS.items():
        status, body = call("GET", f"/v1/{BASE}/c/d", auth=auth)
        entry["reads"][name] = "allow" if status == 200 else ("deny" if status == 403 else {"status": status, "body": body[:200]})
    results.append(entry)

json.dump({"now": "the time of the run (tokens are relative to it)", "tokens": list(TOKENS), "results": results},
          sys.stdout, indent=1)
print()
