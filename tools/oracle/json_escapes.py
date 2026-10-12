"""How does the official emulator escape the strings of its JSON? (#191)

Usage:
    python3 -I tools/oracle/json_escapes.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/json_escapes.json

Writes TEXT, a string of the characters JSON writers may escape, as a field name, a map key, a
string value, a document ID (and so in a reference value), a collection ID and an aggregation
alias, reads each back, and has the emulator refuse requests whose messages repeat it:

- `rest`: cases in the format of `tools/oracle/rest.py`'s fixture, with the same normalisation;
  `crates/hidane/tests/rest.rs` replays them and compares the bodies byte for byte.
- `webchannel`: steps in the format of `tools/oracle/webchannel.py`'s fixture, a Listen and a
  Write session over the same data, except that each read records the exact JSON text of every
  message but `noop` (server times, resume tokens and stream IDs masked);
  `crates/hidane/tests/webchannel.rs` replays them.

What v1.22.0 answers: three escapings.

- REST bodies (protobuf-java's JsonFormat): `"` and `\\` as `\\"` and `\\\\`; `\\b`, `\\t`,
  `\\n`, `\\f` and `\\r`; the other characters below U+0020, U+2028 and U+2029 as `\\u` and four
  lowercase hex digits; everything else, `/` included, as is.
- REST errors: `"`, `\\` and the five as above, `/` as `\\/`, and the other characters of
  U+0000–U+001F, U+007F–U+009F and U+2000–U+20FF as `\\u` and four uppercase hex digits.
- WebChannel messages: as REST bodies, and in strings (not in bytes) also `<`, `>`, `&`, `=` and
  `'` as `\\u003c`, `\\u003e`, `\\u0026`, `\\u003d` and `\\u0027`. A stream's error message is
  escaped as REST bodies are.
"""

import http.client
import json
import re
import sys
import time
import urllib.parse
from urllib.parse import quote

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"

# The characters below U+0020, those escaped for HTML, the C1 controls and the edges of the
# ranges around them, format characters (bidirectional controls, zero widths, the byte order
# mark), U+2028 and U+2029, noncharacters and a character outside the BMP.
TEXT = "".join(map(chr, range(0x20))) + "\"\\/<>&='" + "".join(map(chr, [
    0x7E, 0x7F, 0x80, 0x85, 0x9F, 0xA0, 0xAD, 0x61C, 0x180E, 0x1FFF, 0x2000, 0x200B, 0x200C,
    0x200D, 0x200E, 0x200F, 0x2027, 0x2028, 0x2029, 0x202A, 0x202E, 0x205F, 0x2060, 0x2064,
    0x20AC, 0x20FF, 0x2100, 0x3000, 0xFEFF, 0xFFF9, 0xFFFD, 0xFFFF, 0x1F600,
]))
# A document or collection ID cannot hold `/`.
ID = TEXT.replace("/", "")


def i(n): return {"integerValue": str(n)}
def s(x): return {"stringValue": x}


def fields(name):
    """A document holding TEXT as a field name, a map key, a string value, and in a reference
    value; with bytes, whose base64 holds `+`, `/` and `=`. In name order (#108)."""
    return {
        TEXT: i(1),
        "b": {"bytesValue": "+/8="},
        "m": {"mapValue": {"fields": {TEXT: i(2)}}},
        "r": {"referenceValue": name + "/i/" + ID},
        "s": s(TEXT),
    }


# --- REST -------------------------------------------------------------------------------------

PROJECT = f"json-{time.strftime('%H%M%S')}"
OWNER = {"Authorization": "Bearer owner"}
B = "/v1/projects/{project}/databases/(default)/documents"
NAME = "projects/{project}/databases/(default)/documents"

CASES = []
def case(name, method, path, body=None, headers=OWNER):
    c = {"name": name, "method": method, "path": path, "headers": headers, "compare": "exact"}
    if body is not None:
        c["body"] = body
    CASES.append(c)

case("commit the characters in a field name, a map key, a string, a document ID and a collection ID", "POST", B + ":commit", {"writes": [
    {"update": {"name": NAME + "/c/a", "fields": fields(NAME)}},
    {"update": {"name": NAME + "/i/" + ID, "fields": {"s": s(TEXT)}}},
    {"update": {"name": NAME + "/k" + ID + "/d", "fields": {}}},
]})
case("PATCH answers with the document", "PATCH", B + "/c/p", {"fields": {"s": s(TEXT)}})
case("get the document", "GET", B + "/c/a")
case("get the document with the characters in its ID", "GET", B + "/i/" + quote(ID, safe=""))
case("list it", "GET", B + "/i")
case("batchGet it and a missing one", "POST", B + ":batchGet", {"documents": [NAME + "/i/" + ID, NAME + "/i/" + ID + "x"]})
case("runQuery", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}]}})
case("runQuery over the document with the characters in its ID", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "i"}]}})
case("runAggregationQuery with the characters as an alias", "POST", B + ":runAggregationQuery", {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "i"}]}, "aggregations": [{"count": {}, "alias": TEXT}]}})
case("listCollectionIds", "POST", B + ":listCollectionIds", {})
case("an error that repeats a document name", "GET", B + "/i/" + quote(ID + "x", safe=""))
case("an error that repeats a field name", "POST", B + ":commit", {"writes": [{"update": {"name": NAME + "/c/r", "fields": {"__" + TEXT + "__": i(1)}}}]})
case("an error that repeats a collection ID", "POST", B + ":runQuery", {"structuredQuery": {"from": [{"collectionId": "__" + ID + "__"}]}})


def norm(text):
    text = text.replace(PROJECT, "{project}")
    text = re.sub(r'"(createTime|updateTime|readTime|commitTime)": "[^"]*"', r'"\1": "<time>"', text)
    return text


def rest():
    out = []
    for c in CASES:
        conn = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
        path = c["path"].replace("{project}", PROJECT)
        body = json.dumps(c["body"]).replace("{project}", PROJECT) if "body" in c else None
        headers = dict(c["headers"])
        if body is not None:
            headers["Content-Type"] = "application/json"
        conn.request(c["method"], path, body=body, headers=headers)
        r = conn.getresponse()
        out.append(dict(c, outcome={"status": r.status, "contentType": r.getheader("content-type"), "body": norm(r.read().decode())}))
    return out


# --- WebChannel -------------------------------------------------------------------------------

DB = "projects/json-escapes/databases/(default)"
DOCS = f"{DB}/documents"
HEADERS = "X-Goog-Api-Client:gl-js/ fire/13.0.0\r\nContent-Type:text/plain\r\nx-goog-api-key:demo\r\n"

STEPS = [
    {"op": "commit", "writes": [
        {"update": {"name": f"{DOCS}/c/a", "fields": fields(DOCS)}},
        {"update": {"name": f"{DOCS}/i/{ID}", "fields": {"s": s(TEXT)}}},
    ]},
    {"op": "handshake", "session": "listen", "rpc": "Listen",
     "messages": [{"database": DB, "addTarget": {"documents": {"documents": [f"{DOCS}/c/a"]}, "targetId": 2}}]},
    {"op": "open", "session": "listen", "ci": 0},
    {"op": "read", "session": "listen", "until": "resumeToken"},
    {"op": "forward", "session": "listen", "messages": [{"database": DB, "addTarget": {"documents": {"documents": [f"{DOCS}/i/{ID}"]}, "targetId": 4}}]},
    {"op": "read", "session": "listen", "until": "resumeToken"},
    # A target the emulator refuses: the cause repeats the collection ID.
    {"op": "forward", "session": "listen", "messages": [{"database": DB, "addTarget": {"query": {"parent": DOCS, "structuredQuery": {"from": [{"collectionId": f"__{ID}__"}]}}, "targetId": 6}}]},
    {"op": "read", "session": "listen", "until": "REMOVE"},
    {"op": "terminate", "session": "listen"},
    {"op": "handshake", "session": "write", "rpc": "Write", "messages": [{"database": DB}]},
    {"op": "open", "session": "write", "ci": 0},
    {"op": "read", "session": "write", "until": "noop"},
    {"op": "forward", "session": "write", "messages": [{"streamToken": "MA==", "writes": [{"update": {"name": f"{DOCS}/c/w", "fields": {"s": s(TEXT)}}}]}]},
    {"op": "read", "session": "write", "until": "writeResults"},
    # A write the emulator refuses: the stream's error repeats the field name.
    {"op": "forward", "session": "write", "messages": [{"streamToken": "MQ==", "writes": [{"update": {"name": f"{DOCS}/c/r", "fields": {f"__{TEXT}__": i(1)}}}]}]},
    {"op": "read", "session": "write", "until": "error"},
    {"op": "terminate", "session": "write"},
]


class Frames:
    """Reads `<length>\\n<JSON>` frames from a streaming response, the length in UTF-16 code
    units; each as its exact text."""

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
                    json.loads(text[:i])
                    return text[:i]
            if self.ended or time.monotonic() > deadline:
                return None
            data = self.response.read1(65536)
            if not data:
                self.ended = True
                continue
            self.buffer += data.decode("utf-8")


def payloads(frame):
    """The entries of a frame, `[[id,[message]],…]`, as the exact text of each `[message]`."""
    out, depth, start, in_string, escaped = [], 0, 0, False, False
    for i, ch in enumerate(frame):
        if in_string:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == '"':
                in_string = False
        elif ch == '"':
            in_string = True
        elif ch == "[":
            depth += 1
            if depth == 3:
                start = i
        elif ch == "]":
            if depth == 3:
                out.append(frame[start:i + 1])
            depth -= 1
    return out


def mask(text):
    text = re.sub(r'"(createTime|updateTime|readTime|commitTime)":"[^"]*"', r'"\1":"<time>"', text)
    return re.sub(r'"(resumeToken|streamId)":"[^"]*"', r'"\1":"<\1>"', text)


def query(params):
    return urllib.parse.urlencode(params, safe="()")


def webchannel():
    sessions, outcomes = {}, []
    path = lambda rpc: f"/google.firestore.v1.Firestore/{rpc}/channel"
    for step in STEPS:
        op = step["op"]
        c = http.client.HTTPConnection(*HOST.split(":"), timeout=30)
        if op == "commit":
            c.request("POST", f"/v1/{DOCS}:commit", body=json.dumps({"writes": step["writes"]}),
                      headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
            r = c.getresponse()
            r.read()
            outcomes.append({"status": r.status})
            time.sleep(0.3)
        elif op == "handshake":
            body = {"headers": HEADERS, "count": len(step["messages"]), "ofs": 0}
            for n, m in enumerate(step["messages"]):
                body[f"req{n}___data__"] = json.dumps(m)
            c.request("POST", path(step["rpc"]) + "?" + query({"VER": 8, "database": DB, "RID": 1000, "CVER": 22, "X-HTTP-Session-Id": "gsessionid", "zx": "x", "t": 1}),
                      body=urllib.parse.urlencode(body), headers={"Content-Type": "application/x-www-form-urlencoded"})
            r = c.getresponse()
            frame = json.loads(Frames(r).next(time.monotonic() + 10))
            sid = frame[0][1][1]
            sessions[step["session"]] = {"rpc": step["rpc"], "sid": sid, "rid": 1001, "aid": 0, "ofs": len(step["messages"]), "frames": None}
            frame[0][1][1] = "<sid>"
            outcomes.append({"status": r.status, "frame": frame})
            time.sleep(0.3)
        elif op == "open":
            s = sessions[step["session"]]
            c.request("GET", path(s["rpc"]) + "?" + query({"VER": 8, "database": DB, "RID": "rpc", "SID": s["sid"], "AID": s["aid"], "CI": step["ci"], "TYPE": "xmlhttp", "zx": "x", "t": 1}))
            r = c.getresponse()
            s["frames"] = Frames(r)
            outcomes.append({"status": r.status})
        elif op == "read":
            s = sessions[step["session"]]
            texts, deadline = [], time.monotonic() + 10
            while True:
                frame = s["frames"].next(deadline)
                if frame is None:
                    break
                for entry in json.loads(frame):
                    s["aid"] = entry[0]
                texts += [mask(p) for p in payloads(frame) if p != '["noop"]']
                until = step["until"]
                if ('"noop"' in frame) if until == "noop" else (until in frame):
                    break
            outcomes.append({"texts": texts, "ended": s["frames"].ended})
        elif op == "forward":
            s = sessions[step["session"]]
            body = {"count": len(step["messages"]), "ofs": s["ofs"]}
            for n, m in enumerate(step["messages"]):
                body[f"req{n}___data__"] = json.dumps(m)
            s["ofs"] += len(step["messages"])
            c.request("POST", path(s["rpc"]) + "?" + query({"VER": 8, "database": DB, "SID": s["sid"], "RID": s["rid"], "AID": s["aid"], "zx": "x", "t": 1}),
                      body=urllib.parse.urlencode(body), headers={"Content-Type": "application/x-www-form-urlencoded"})
            s["rid"] += 1
            r = c.getresponse()
            outcomes.append({"status": r.status, "frame": json.loads(Frames(r).next(time.monotonic() + 10))})
            time.sleep(0.3)
        elif op == "terminate":
            s = sessions[step["session"]]
            c.request("POST", path(s["rpc"]) + "?" + query({"VER": 8, "database": DB, "SID": s["sid"], "RID": s["rid"], "TYPE": "terminate", "zx": "x"}),
                      headers={"Content-Type": "text/plain;charset=UTF-8"})
            r = c.getresponse()
            outcomes.append({"status": r.status, "body": r.read().decode()})
    return [{**step, "outcome": o} for step, o in zip(STEPS, outcomes)]


json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/json_escapes.py",
    "text": TEXT,
    "rest": rest(),
    "webchannel": {"database": DB, "steps": webchannel()},
}, sys.stdout, ensure_ascii=False, indent=1)
print()
