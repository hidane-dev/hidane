#!/usr/bin/env python3
"""How does the request monitor (`--websocket_port`) write strings that JSON may escape? (#198)

Usage (from the repository root), with a freshly started jar under an English locale, given a
WebSocket port:
    (cd /tmp && java -Duser.language=en -Duser.country=US -jar …/cloud-firestore-emulator-v1.22.0.jar \\
        --host 127.0.0.1 --port 8086 --websocket_port 8087) &
    python3 -I tools/oracle/websocket_escapes.py 127.0.0.1:8086 8087 \\
        > crates/hidane/tests/fixtures/websocket_escapes.json

The steps are REST calls, as in `websocket_requests.py`, but a step keeps the text frames the
monitor client received after it as they came (`frames`), so that a test sees how each string is
written and not only what it reads as. The strings hold HTML characters (`<`, `>`, `&`, `=`,
`'`), which the official emulator's WebChannel escapes, quotes and backslashes, control
characters, U+007F and U+0080, U+2028 and U+2029, and characters outside the Basic Multilingual
Plane: in the ruleset's text, a document ID and so the path, map keys, string values, a reference,
bytes (base64 with `+`, `/` and `=`) and the claims of a token. Stdlib only.
"""

import base64
import json
import os
import socket
import struct
import sys
import urllib.parse
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from steps import call, case, cases, dump, rules  # noqa: E402

HOST = sys.argv[1]
WS_PORT = int(sys.argv[2])
WS_HOST = HOST.rsplit(":", 1)[0]
PROJECT = "websocket-escapes"
DOCS = f"/v1/projects/{PROJECT}/databases/(default)/documents"
# Everything a JSON writer might escape, but an unpaired surrogate (D083).
SPECIAL = "<>&='\"\\/    \x7f\x80é \U0001F600 \x00\x01\x1f\b\t\n\f\r"
ID = "d<>&='  é\U0001F600"


def b64(obj):
    return base64.urlsafe_b64encode(json.dumps(obj).encode()).decode().rstrip("=")


TOKEN = "Bearer " + b64({"alg": "none"}) + "." + b64({"sub": "u<>&=' ", "name": SPECIAL}) + "."


class Client:
    """A WebSocket client, as in websocket_requests.py, that keeps text frames as text."""

    def __init__(self, path="/requests"):
        self.sock = socket.create_connection((WS_HOST, WS_PORT), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f"GET {path} HTTP/1.1\r\nHost: {WS_HOST}:{WS_PORT}\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        self.buf = b""
        while b"\r\n\r\n" not in self.buf:
            chunk = self.sock.recv(65536)
            assert chunk, "closed during the handshake"
            self.buf += chunk
        _, _, self.buf = self.buf.partition(b"\r\n\r\n")

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def frames(self, wait=1.0):
        """The text frames received until none came for `wait` seconds, as text."""
        out = []
        self.sock.settimeout(wait)
        try:
            while True:
                b0, b1 = self._read(2)
                n = b1 & 127
                if n == 126:
                    n = struct.unpack(">H", self._read(2))[0]
                elif n == 127:
                    n = struct.unpack(">Q", self._read(8))[0]
                assert b0 == 0x81 and not b1 & 128, "not a whole text frame from the server"
                out.append(self._read(n).decode())
        except (socket.timeout, TimeoutError):
            pass
        return out


def rest(method, path, body=None, auth=None):
    """A REST call, with the text frames the monitor received after it."""
    status, text = call(method, path, body, auth=auth)
    assert status == 200, text
    cases[-1]["steps"][-1]["frames"] = MONITOR.frames()


MONITOR = Client()
MONITOR.frames(0.5)

case("strings in a record", project=PROJECT)
rules(PROJECT, "rules_version = '2';\n"
      f"// {SPECIAL.replace(chr(10), ' ').replace(chr(13), ' ')}\n"
      "service cloud.firestore {\n  match /databases/{database}/documents {\n"
      "    match /{document=**} {\n      allow read, write: if true;\n    }\n  }\n}\n")
cases[-1]["steps"][-1]["frames"] = MONITOR.frames()
name = f"projects/{PROJECT}/databases/(default)/documents/c/{ID}"
rest("POST", f"{DOCS}:commit", {"writes": [{
    "update": {"name": name, "fields": {
        "s": {"stringValue": SPECIAL},
        f"k{SPECIAL}": {"mapValue": {"fields": {SPECIAL: {"stringValue": "v"}}}},
        "a": {"arrayValue": {"values": [{"stringValue": "<a href='x'>"}, {"stringValue": " "}]}},
        "r": {"referenceValue": name},
        "b": {"bytesValue": base64.b64encode(b"\xfb\xff\xbe").decode()},
        "p": {"bytesValue": base64.b64encode(b"\xfb\xff").decode()},
    }},
    "currentDocument": {"exists": False},
}]}, auth=TOKEN)
rest("GET", f"{DOCS}/c/{urllib.parse.quote(ID, safe='')}", auth=TOKEN)
dump()
