#!/usr/bin/env python3
"""What does the request monitor (`--websocket_port`) send for a decision that reaches the limit of 1,000 expressions? (#293)

Usage (from the repository root), with a freshly started jar under an English locale, given a
WebSocket port:
    (cd /tmp && java -Duser.language=en -Duser.country=US -jar …/cloud-firestore-emulator-v1.22.0.jar \\
        --host 127.0.0.1 --port 8086 --websocket_port 8087) &
    python3 -I tools/oracle/rules_expressions_monitor.py 127.0.0.1:8086 8087 \\
        > crates/hidane/tests/fixtures/rules_expressions_monitor.json

The recording has the format of `websocket_requests.py`'s: a monitor client connects first, and
each REST call (made without an `Authorization` header) records what it then received. Reads
allowed and denied within the limit are recorded as usual. The official emulator sends no record
of the read that reaches the limit; the client connected before it still gets the records of
the reads after it, but a client that connects after it gets nothing: no backlog, and no record.
Those steps are marked `differs`: hidane records every decision and sends every client its
records (`docs/parity-exceptions.md`). The jar must be fresh: its monitor does not come back.
Stdlib only.
"""

import base64
import json
import os
import socket
import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from steps import call, case, cases, dump, rules  # noqa: E402

HOST = sys.argv[1]
WS_PORT = int(sys.argv[2])
WS_HOST = HOST.rsplit(":", 1)[0]
PROJECT = "expressions-monitor"
DOCS = f"/v1/projects/{PROJECT}/databases/(default)/documents"
# Functions whose calls cost 2, 7, 17, 37, 77, 157, 317 and 637 expressions, as in
# rules_expressions.py.
COSTS = [2]
for _ in range(7):
    COSTS.append(2 * COSTS[-1] + 3)
COST_FUNCTIONS = "function d0() { return true; }\n" + "\n".join(
    f"function d{k}() {{ return d{k - 1}() && d{k - 1}(); }}" for k in range(1, len(COSTS)))


def cost(n):
    """An expression that costs `n` (at least 1) and is true."""
    if n <= 4:
        return ["true", "!false", "!!true", "!!!false"][n - 1]
    k = max(k for k, c in enumerate(COSTS) if c + 3 <= n)
    return f"d{k}() && {cost(n - COSTS[k] - 2)}"


class Client:
    """A WebSocket client, as in websocket_requests.py: the handshake, then frames, unmasked
    from the server."""

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
        head, _, self.buf = self.buf.partition(b"\r\n\r\n")
        lines = head.decode().split("\r\n")
        self.status = lines[0]
        self.headers = [line.split(": ", 1)[0] for line in lines[1:]]

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(1 << 20)
            if not chunk:
                raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def _frame(self):
        b0, b1 = self._read(2)
        n = b1 & 127
        if n == 126:
            n = struct.unpack(">H", self._read(2))[0]
        elif n == 127:
            n = struct.unpack(">Q", self._read(8))[0]
        assert not b1 & 128, "a masked frame from the server"
        return {"fin": b0 >> 7, "opcode": b0 & 15, "data": self._read(n)}

    def frames(self, wait=1.0):
        """The frames received until none came for `wait` seconds: text as JSON, others as
        opcode and payload, and `"EOF"` when the server closed."""
        out = []
        self.sock.settimeout(wait)
        try:
            while True:
                f = self._frame()
                assert f["fin"], "a fragmented message"
                if f["opcode"] == 1:
                    out.append(json.loads(f["data"]))
                else:
                    out.append({"opcode": f["opcode"], "payload": f["data"].decode("latin-1")})
        except (socket.timeout, TimeoutError):
            pass
        except EOFError:
            out.append("EOF")
        return out


def ws(**step):
    cases[-1]["steps"].append({"ws": step.pop("action"), **step})


def read(path, **step):
    """A read, with the records the monitor received after it."""
    call("GET", f"{DOCS}/{path}")
    recorded = cases[-1]["steps"][-1]
    recorded["records"] = MONITOR.frames()
    recorded.update(step)


UNRECORDED = "the official emulator does not record a decision past the limit"
SILENT = "the official emulator sends nothing to a client that connects after a decision past the limit"
case("a decision past the limit", project=PROJECT)
MONITOR = Client()
ws(action="connect", client="monitor", status=MONITOR.status, headers=MONITOR.headers, frames=MONITOR.frames())
rules(PROJECT, "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
      + "\n".join("    " + line for line in COST_FUNCTIONS.split("\n")) + "\n"
      + f"    match /at/{{id}} {{\n      allow get: if {cost(1000)};\n    }}\n"
      + f"    match /past/{{id}} {{\n      allow get: if {cost(1001)};\n    }}\n"
      + "    match /no/{id} {\n      allow get: if false;\n    }\n  }\n}\n")
read("at/x")
read("no/x")
read("past/x", differs=UNRECORDED)
read("at/x")
read("no/x")
late = Client()
ws(action="connect", client="late", status=late.status, headers=late.headers, frames=late.frames(), differs=SILENT)
read("at/x")
ws(action="receive", client="late", frames=late.frames(), differs=SILENT)
dump()
