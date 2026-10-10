"""How does the official emulator speak WebChannel, the browser SDK's transport? (#71, #87)

Usage:
    python3 -I tools/oracle/webchannel.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/webchannel.json

Speaks the protocol the way firebase-js-sdk does (`VER=8`; the wire is described in
docs/webchannel.md) and records each step's outcome:

- `handshake`: `POST …/{Listen,Write}/channel` with the first messages in a form body;
  the answer `[[0,["c","<sid>","",8,12,30000]]]`.
- `open` / `read`: a back channel `GET …&TYPE=xmlhttp` (`CI=0` streams, `CI=1` long-polls);
  the messages read, as `[id, payload]`, until a matching one, or until the response ends.
- `forward`: more messages in a `POST`; the answer `[<1 with a back channel>,<last id>,7]`.
- `terminate`, `unknown` (a session that does not exist), `commit` (REST, to change data).

Frames are checked as they are read: each is `<length>\n<JSON>`, the length in UTF-16 code
units. Server times, resume tokens, session and stream IDs are replaced by placeholders.
`crates/hidane/tests/webchannel.rs` replays the steps against hidane.
"""

import http.client
import json
import re
import sys
import time
import urllib.parse

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
PROJECT = "webchannel"
DB = f"projects/{PROJECT}/databases/(default)"
DOCS = f"{DB}/documents"
HEADERS = "X-Goog-Api-Client:gl-js/ fire/13.0.0\r\nContent-Type:text/plain\r\nx-goog-api-key:demo\r\n"
LARGE = "x" * 1048488
TEXT = "café \U0001F600"

STEPS = [
    # A document target on a missing document, then the document changing.
    {"op": "handshake", "session": "listen", "rpc": "Listen",
     "messages": [{"database": DB, "addTarget": {"documents": {"documents": [f"{DOCS}/c/a"]}, "targetId": 2}}]},
    {"op": "open", "session": "listen", "ci": 0},
    {"op": "read", "session": "listen", "until": "noop"},
    {"op": "commit", "writes": [{"update": {"name": f"{DOCS}/c/a", "fields": {"s": {"stringValue": TEXT}}}}]},
    {"op": "read", "session": "listen", "until": "resumeToken"},
    {"op": "forward", "session": "listen", "messages": [{"database": DB, "removeTarget": 2}]},
    {"op": "read", "session": "listen", "until": "REMOVE"},
    {"op": "forward", "session": "listen", "messages": [{"database": DB, "addTarget": {"query": {"parent": DOCS, "structuredQuery": {"from": [{"collectionId": "c"}]}}, "targetId": 4}}]},
    {"op": "read", "session": "listen", "until": "resumeToken"},
    {"op": "terminate", "session": "listen"},
    {"op": "unknown", "method": "GET", "rpc": "Listen"},
    {"op": "unknown", "method": "POST", "rpc": "Listen"},
    {"op": "unknown", "method": "terminate", "rpc": "Listen"},
    # Writes, then one the server refuses: the stream's error is a message.
    {"op": "handshake", "session": "write", "rpc": "Write", "messages": [{"database": DB}]},
    {"op": "open", "session": "write", "ci": 0},
    {"op": "read", "session": "write", "until": "noop"},
    {"op": "forward", "session": "write", "messages": [{"streamToken": "MA==", "writes": [{"update": {"name": f"{DOCS}/c/b", "fields": {"n": {"integerValue": "1"}}}}]}]},
    {"op": "read", "session": "write", "until": "writeResults"},
    {"op": "forward", "session": "write", "messages": [{"streamToken": "MQ==", "writes": [{"update": {"name": f"{DOCS}/c/big", "fields": {"s": {"stringValue": LARGE}}}}]}]},
    {"op": "read", "session": "write", "until": "error"},
    {"op": "terminate", "session": "write"},
    # Long polling: each back channel ends with the first data.
    {"op": "handshake", "session": "poll", "rpc": "Listen",
     "messages": [{"database": DB, "addTarget": {"documents": {"documents": [f"{DOCS}/c/p"]}, "targetId": 2}}]},
    {"op": "open", "session": "poll", "ci": 1},
    {"op": "read", "session": "poll", "until": "end"},
    {"op": "open", "session": "poll", "ci": 1},
    {"op": "read", "session": "poll", "until": "noop"},
    {"op": "commit", "writes": [{"update": {"name": f"{DOCS}/c/p", "fields": {"n": {"integerValue": "1"}}}}]},
    {"op": "read", "session": "poll", "until": "end"},
    {"op": "terminate", "session": "poll"},
    # A Write session that ends with a handshake carrying no message.
    {"op": "handshake", "session": "empty", "rpc": "Write", "messages": []},
    {"op": "forward", "session": "empty", "messages": [{"database": DB}]},
    {"op": "open", "session": "empty", "ci": 0},
    {"op": "read", "session": "empty", "until": "noop"},
    {"op": "terminate", "session": "empty"},
]


def utf16_len(text):
    return len(text.encode("utf-16-le")) // 2


class Frames:
    """Reads `<length>\\n<JSON>` frames from a streaming response."""

    def __init__(self, response):
        self.response, self.buffer, self.ended = response, "", False

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
            if self.ended or time.monotonic() > deadline:
                return None
            data = self.response.read1(65536)
            if not data:
                self.ended = True
                continue
            self.buffer += data.decode("utf-8")


def mask(value):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if k.endswith("Time"):
                out[k] = "<time>"
            elif k in ("resumeToken", "streamId"):
                out[k] = f"<{k}>"
            else:
                out[k] = mask(v)
        return out
    if isinstance(value, list):
        return [mask(v) for v in value]
    if isinstance(value, str) and len(value) > 64:
        return f"<{len(value)} characters>"
    return value


def query(params):
    return urllib.parse.urlencode(params, safe="()")


def run():
    sessions, outcomes = {}, []
    for step in STEPS:
        op = step["op"]
        path = lambda rpc: f"/google.firestore.v1.Firestore/{rpc}/channel"
        if op == "handshake":
            body = {"headers": HEADERS, "count": len(step["messages"]), "ofs": 0}
            for i, m in enumerate(step["messages"]):
                body[f"req{i}___data__"] = json.dumps(m)
            c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
            c.request("POST", path(step["rpc"]) + "?" + query({"VER": 8, "database": DB, "RID": 1000, "CVER": 22, "X-HTTP-Session-Id": "gsessionid", "zx": "x", "t": 1}),
                      body=urllib.parse.urlencode(body), headers={"Content-Type": "application/x-www-form-urlencoded"})
            r = c.getresponse()
            frame = Frames(r).next(time.monotonic() + 10)
            sid = frame[0][1][1]
            sessions[step["session"]] = {"rpc": step["rpc"], "sid": sid, "rid": 1001, "aid": 0, "ofs": len(step["messages"]), "frames": None}
            frame[0][1][1] = "<sid>"
            outcomes.append({"status": r.status, "frame": frame})
            time.sleep(0.3)
        elif op == "open":
            s = sessions[step["session"]]
            c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
            c.request("GET", path(s["rpc"]) + "?" + query({"VER": 8, "database": DB, "RID": "rpc", "SID": s["sid"], "AID": s["aid"], "CI": step["ci"], "TYPE": "xmlhttp", "zx": "x", "t": 1}))
            r = c.getresponse()
            s["frames"] = Frames(r)
            outcomes.append({"status": r.status})
        elif op == "read":
            s = sessions[step["session"]]
            read, deadline = [], time.monotonic() + 10
            while True:
                frame = s["frames"].next(deadline)
                if frame is None:
                    break
                for entry in frame:
                    s["aid"] = entry[0]
                    read.append(mask(entry))
                last = json.dumps(frame)
                until = step["until"]
                if until != "end" and (('"noop"' in last) if until == "noop" else (until in last)):
                    break
            outcomes.append({"messages": read, "ended": s["frames"].ended})
        elif op == "forward":
            s = sessions[step["session"]]
            body = {"count": len(step["messages"]), "ofs": s["ofs"]}
            for i, m in enumerate(step["messages"]):
                body[f"req{i}___data__"] = json.dumps(m)
            s["ofs"] += len(step["messages"])
            c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
            c.request("POST", path(s["rpc"]) + "?" + query({"VER": 8, "database": DB, "SID": s["sid"], "RID": s["rid"], "AID": s["aid"], "zx": "x", "t": 1}),
                      body=urllib.parse.urlencode(body), headers={"Content-Type": "application/x-www-form-urlencoded"})
            s["rid"] += 1
            r = c.getresponse()
            outcomes.append({"status": r.status, "frame": Frames(r).next(time.monotonic() + 10)})
            time.sleep(0.3)
        elif op == "terminate":
            s = sessions[step["session"]]
            c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
            c.request("POST", path(s["rpc"]) + "?" + query({"VER": 8, "database": DB, "SID": s["sid"], "RID": s["rid"], "TYPE": "terminate", "zx": "x"}),
                      headers={"Content-Type": "text/plain;charset=UTF-8"})
            r = c.getresponse()
            outcomes.append({"status": r.status, "body": r.read().decode()})
        elif op == "unknown":
            params = {"VER": 8, "database": DB, "SID": "no-such-session", "RID": 5, "AID": 0, "zx": "x"}
            method = "POST"
            if step["method"] == "GET":
                params.update({"RID": "rpc", "CI": 0, "TYPE": "xmlhttp"})
                method = "GET"
            elif step["method"] == "terminate":
                params["TYPE"] = "terminate"
            c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
            c.request(method, path(step["rpc"]) + "?" + query(params), body=None if method == "GET" else "count=0&ofs=0",
                      headers={"Content-Type": "application/x-www-form-urlencoded"})
            r = c.getresponse()
            outcomes.append({"status": r.status, "body": r.read().decode()})
        elif op == "commit":
            c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
            c.request("POST", f"/v1/{DOCS}:commit", body=json.dumps({"writes": step["writes"]}),
                      headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
            r = c.getresponse()
            r.read()
            outcomes.append({"status": r.status})
            time.sleep(0.3)
    return outcomes


c = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
c.request("POST", "/reset")
c.getresponse().read()
outcomes = run()
json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/webchannel.py",
    "steps": [{**{k: v for k, v in step.items()}, "outcome": o} for step, o in zip(STEPS, outcomes)],
}, sys.stdout, ensure_ascii=False, indent=1)
print()
