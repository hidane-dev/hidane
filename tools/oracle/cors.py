"""How does the official emulator answer CORS? (#37)

Usage:
    python3 -I tools/oracle/cors.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/cors.json

Sends preflights and ordinary requests with and without `Origin` to REST, admin and unknown
paths, and records the status, every response header but `date` and `content-length`, and the
body. `crates/hidane/tests/cors.rs` sends the same requests to hidane and compares.
"""

import http.client
import json
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
D = "/v1/projects/cors/databases/(default)/documents"
O = "http://localhost:3000"
CASES = [
    ("preflight", "OPTIONS", D + "/c/a", {"Origin": O, "Access-Control-Request-Method": "PATCH", "Access-Control-Request-Headers": "content-type,authorization,x-goog-api-client"}, None),
    ("preflight without request headers", "OPTIONS", D + "/c/a", {"Origin": O, "Access-Control-Request-Method": "GET"}, None),
    ("preflight for a private network", "OPTIONS", D + "/c/a", {"Origin": O, "Access-Control-Request-Method": "GET", "Access-Control-Request-Private-Network": "true"}, None),
    ("private network asked false", "OPTIONS", D + "/c/a", {"Origin": O, "Access-Control-Request-Method": "GET", "Access-Control-Request-Private-Network": "false"}, None),
    ("private network and headers without an origin", "OPTIONS", D + "/c/a", {"Access-Control-Request-Method": "GET", "Access-Control-Request-Headers": "content-type", "Access-Control-Request-Private-Network": "true"}, None),
    ("OPTIONS without a request method", "OPTIONS", D + "/c/a", {"Origin": O}, None),
    ("OPTIONS without an origin", "OPTIONS", D + "/c/a", {}, None),
    ("preflight on an unknown path", "OPTIONS", "/nope", {"Origin": O, "Access-Control-Request-Method": "GET"}, None),
    ("preflight on /reset", "OPTIONS", "/reset", {"Origin": O, "Access-Control-Request-Method": "POST"}, None),
    ("preflight on /emulator/v1", "OPTIONS", "/emulator/v1/projects/cors/databases/(default)/documents", {"Origin": O, "Access-Control-Request-Method": "DELETE"}, None),
    ("preflight on a verb", "OPTIONS", D + ":commit", {"Origin": O, "Access-Control-Request-Method": "POST", "Access-Control-Request-Headers": "content-type"}, None),
    ("preflight on the WebChannel path", "OPTIONS", "/google.firestore.v1.Firestore/Listen/channel?VER=8", {"Origin": O, "Access-Control-Request-Method": "POST", "Access-Control-Request-Headers": "content-type,x-goog-api-client"}, None),
    ("GET with an origin", "GET", D + "/c/a", {"Origin": O}, None),
    ("GET with another origin", "GET", D + "/c/a", {"Origin": "https://example.com"}, None),
    ("GET with origin null", "GET", D + "/c/a", {"Origin": "null"}, None),
    ("GET without an origin", "GET", D + "/c/a", {}, None),
    ("POST :commit with an origin", "POST", D + ":commit", {"Origin": O, "Content-Type": "text/plain"}, "{}"),
    ("GET / with an origin", "GET", "/", {"Origin": O}, None),
    ("DELETE /emulator/v1 with an origin", "DELETE", "/emulator/v1/projects/cors/databases/(default)/documents", {"Origin": O}, None),
    ("404 with an origin", "GET", "/nope", {"Origin": O}, None),
    ("HEAD with an origin", "HEAD", D + "/c/a", {"Origin": O}, None),
    ("POST /reset with an origin", "POST", "/reset", {"Origin": O}, None),
]

out = []
for name, method, path, headers, body in CASES:
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
    c.request(method, path, body=body, headers=headers)
    r = c.getresponse()
    data = r.read().decode()
    out.append({
        "name": name, "method": method, "path": path, "headers": headers, "body": body,
        "outcome": {
            "status": r.status,
            "headers": dict(sorted((k.lower(), v) for k, v in r.getheaders() if k.lower() not in ("date", "content-length"))),
            "body": data,
        },
    })

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/cors.py",
    "cases": out,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
