#!/usr/bin/env python3
"""A stand-in for the Compute Engine metadata server, for the REST variant of the system tests.

Over REST, nodejs-firestore asks google-auth-library for credentials even with
`FIRESTORE_EMULATOR_HOST` set. With `GCE_METADATA_HOST` naming this server and
`METADATA_SERVER_DETECTION=assume-present`, the library takes it for the metadata server, and
its access token is `owner`: the SDK sends `Authorization: Bearer owner`, which both emulators
read as the administrator's, as they do over gRPC. Nothing reaches Google.

    python3 -I tools/nodejs-system-tests/metadata.py

Listens on a free port of 127.0.0.1 and prints it on the first line. Stdlib only.
"""

import http.server
import json
import sys

ANSWERS = {
    "/computeMetadata/v1/instance": "",
    "/computeMetadata/v1/instance/service-accounts/default/token":
        json.dumps({"access_token": "owner", "expires_in": 3599, "token_type": "Bearer"}),
    "/computeMetadata/v1/instance/service-accounts/default/email": "emulator@demo-system.iam.gserviceaccount.com",
    "/computeMetadata/v1/project/project-id": "demo-system",
    "/computeMetadata/v1/universe/universe-domain": "googleapis.com",
}


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = ANSWERS.get(self.path.split("?")[0])
        self.send_response(404 if body is None else 200)
        self.send_header("Metadata-Flavor", "Google")
        if body is not None:
            data = body.encode()
            self.send_header("Content-Type", "application/json" if body.startswith("{") else "text/plain")
            self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        if body is not None:
            self.wfile.write(data)

    def log_message(self, *args):
        pass


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
