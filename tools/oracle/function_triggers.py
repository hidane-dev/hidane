"""How does the official emulator answer the Functions emulator's trigger registrations? (#185)

Usage (from the repository root):
    python3 -I tools/oracle/function_triggers.py <cloud-firestore-emulator.jar> [java] [port] \
        > crates/hidane/tests/fixtures/function_triggers.json

When functions with Firestore triggers load, firebase-tools' Functions emulator registers each
trigger with the Firestore emulator (`functionsEmulator.js`, `addFirestoreTrigger`), and stops
the whole suite if a registration fails:

- v1 (`functions.firestore.document(…).onCreate`): `PUT /emulator/v1/projects/{p}/triggers/{key}`
  with `{"eventTrigger": {eventType, resource, service: "firestore.googleapis.com"}}`;
- v2 (`onDocumentCreated`): `POST /emulator/v1/projects/{p}/eventarcTrigger?eventarcTriggerId={key}`
  with `{eventType, database, namespace, document: {value, matchType: EXACT | PATH_PATTERN}}`.

The script starts the jar twice on `port` (default 8131), from a temporary directory and under an
English locale: without `--functions_emulator`, then with `--functions_emulator 127.0.0.1:5001`
(nothing has to listen there for a registration). Against each, it sends the registrations as
firebase-tools sends them, then other event types, projects, databases, keys, bodies (invalid
JSON, missing and unknown fields, values of other types, both spellings of a field), query
parameters, methods, paths, content types and `Authorization` headers, and records the status,
the content type and the body of each answer, with the `WARNING` lines the jar printed for it
(the reason behind a 500, which the answer does not give). No document is written: delivering
events is #70. `crates/hidane/tests/function_triggers.rs` replays the cases against hidane.
"""

import http.client
import json
import os
import subprocess
import sys
import tempfile
import time

JAR = sys.argv[1]
JAVA = sys.argv[2] if len(sys.argv) > 2 else "java"
PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8131
HOST = f"127.0.0.1:{PORT}"
FUNCTIONS = ["--functions_emulator", "127.0.0.1:5001"]

P = "demo-triggers"
T = f"/emulator/v1/projects/{P}/triggers/"
E = f"/emulator/v1/projects/{P}/eventarcTrigger"
JSON = {"Content-Type": "application/json"}
# What firebase-tools' API client sends to an emulator.
TOOLS = {"Content-Type": "application/json", "Authorization": "Bearer owner"}

V1_TYPES = ["providers/cloud.firestore/eventTypes/document." + t for t in ("create", "update", "delete", "write")]
V2_TYPES = [f"google.cloud.firestore.document.v1.{t}{auth}"
            for auth in ("", ".withAuthContext") for t in ("created", "updated", "deleted", "written")]


def compact(value):
    """JSON as `JSON.stringify` writes it, the way firebase-tools sends the bundles."""
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def v1(project=P, **fields):
    trigger = {"eventType": V1_TYPES[0], "resource": f"projects/{project}/databases/(default)/documents/items/{{id}}",
               "service": "firestore.googleapis.com"}
    trigger.update(fields)
    return compact({"eventTrigger": {k: v for k, v in trigger.items() if v is not ...}})


def v2(**fields):
    trigger = {"eventType": V2_TYPES[0], "database": "(default)", "namespace": "(default)",
               "document": {"value": "items/{id}", "matchType": "PATH_PATTERN"}}
    trigger.update(fields)
    return compact({k: v for k, v in trigger.items() if v is not ...})


def doc(**fields):
    return v2(document=fields)


CASES = [
    # (name, method, path, headers, body)
    ("v1, as firebase-tools sends it", "PUT", T + "us-central1-v1created-0", TOOLS, v1()),
    ("v1, the same key again", "PUT", T + "us-central1-v1created-0", TOOLS, v1()),
    ("v1, the same key with another trigger", "PUT", T + "us-central1-v1created-0", TOOLS, v1(eventType=V1_TYPES[3])),
    *[(f"v1, {t.rsplit('/', 1)[1]}", "PUT", T + f"us-central1-v1-{i}", TOOLS, v1(eventType=t)) for i, t in enumerate(V1_TYPES)],
    ("v2, as firebase-tools sends it", "POST", E + "?eventarcTriggerId=us-central1-v2created-0", TOOLS, v2()),
    ("v2, the same key again", "POST", E + "?eventarcTriggerId=us-central1-v2created-0", TOOLS, v2()),
    ("v2, an exact document", "POST", E + "?eventarcTriggerId=us-central1-v2exact-0", TOOLS, doc(value="items/a", matchType="EXACT")),
    *[(f"v2, {t.split('v1.', 1)[1]}", "POST", E + f"?eventarcTriggerId=us-central1-v2-{i}", TOOLS, v2(eventType=t)) for i, t in enumerate(V2_TYPES)],
    ("v2, another database", "POST", E + "?eventarcTriggerId=db", TOOLS, v2(database="other-db")),

    # Projects, databases and keys.
    ("v1, another project", "PUT", "/emulator/v1/projects/other-project/triggers/k", JSON, v1("other-project")),
    ("v1, a domain-scoped project", "PUT", "/emulator/v1/projects/example.com:my-project/triggers/k", JSON, v1()),
    ("v1, a project with a capital letter", "PUT", "/emulator/v1/projects/Demo/triggers/k", JSON, v1()),
    ("v1, a project with an underscore", "PUT", "/emulator/v1/projects/demo_triggers/triggers/k", JSON, v1()),
    ("v1, a project with a dot", "PUT", "/emulator/v1/projects/demo.triggers/triggers/k", JSON, v1()),
    ("v1, a project starting with a hyphen", "PUT", "/emulator/v1/projects/-demo/triggers/k", JSON, v1()),
    ("v1, a project ending with a hyphen", "PUT", "/emulator/v1/projects/demo-/triggers/k", JSON, v1()),
    ("v1, a project of two colons", "PUT", "/emulator/v1/projects/a:b:c/triggers/k", JSON, v1()),
    ("v1, a project of 100 characters", "PUT", "/emulator/v1/projects/" + "p" * 100 + "/triggers/k", JSON, v1()),
    ("v1, a project of 101 characters", "PUT", "/emulator/v1/projects/" + "p" * 101 + "/triggers/k", JSON, v1()),
    ("v1, a project with spaces around", "PUT", "/emulator/v1/projects/%20demo%20/triggers/k", JSON, v1()),
    ("v1, a project with a space inside", "PUT", "/emulator/v1/projects/de%20mo/triggers/k", JSON, v1()),
    ("v1, a project of non-ASCII letters", "PUT", "/emulator/v1/projects/%C3%A9/triggers/k", JSON, v1()),
    ("v1, an empty project", "PUT", "/emulator/v1/projects//triggers/k", JSON, v1()),
    ("v1, a blank project", "PUT", "/emulator/v1/projects/%20/triggers/k", JSON, v1()),
    ("v1, an encoded slash in the project", "PUT", "/emulator/v1/projects/a%2Fb/triggers/k", JSON, v1()),
    ("v1, an empty key", "PUT", T, JSON, v1()),
    ("v1, a blank key", "PUT", T + "%20", JSON, v1()),
    ("v1, a key with a space", "PUT", T + "a%20b", JSON, v1()),
    ("v1, a key with a colon", "PUT", T + "a:b", JSON, v1()),
    ("v1, a key of non-ASCII letters", "PUT", T + "%C3%A9", JSON, v1()),
    ("v1, an encoded slash in the key", "PUT", T + "a%2Fb", JSON, v1()),
    ("v1, a resource in another project", "PUT", T + "k", JSON, v1(resource="projects/other/databases/(default)/documents/items/{id}")),
    ("v1, a resource in another database", "PUT", T + "k", JSON, v1(resource=f"projects/{P}/databases/other-db/documents/items/{{id}}")),
    ("v1, a resource that is not a name", "PUT", T + "k", JSON, v1(resource="nonsense")),
    ("v1, no resource", "PUT", T + "k", JSON, v1(resource=...)),
    ("v1, a query string", "PUT", T + "k?foo=1&parent=x", JSON, v1()),
    ("v2, no eventarcTriggerId", "POST", E, JSON, v2()),
    ("v2, an empty eventarcTriggerId", "POST", E + "?eventarcTriggerId=", JSON, v2()),
    ("v2, a blank eventarcTriggerId", "POST", E + "?eventarcTriggerId=%20", JSON, v2()),
    ("v2, a slash in eventarcTriggerId", "POST", E + "?eventarcTriggerId=a/b", JSON, v2()),
    ("v2, eventarc_trigger_id", "POST", E + "?eventarc_trigger_id=k", JSON, v2()),
    ("v2, eventarcTriggerId twice", "POST", E + "?eventarcTriggerId=a&eventarcTriggerId=a", JSON, v2()),
    ("v2, eventarcTriggerId in both spellings", "POST", E + "?eventarcTriggerId=a&eventarc_trigger_id=b", JSON, v2()),
    ("v2, parent in the query", "POST", E + "?eventarcTriggerId=k&parent=x", JSON, v2()),
    ("v2, another query parameter", "POST", E + "?eventarcTriggerId=k&foo=1", JSON, v2()),
    ("v2, a project with a capital letter", "POST", "/emulator/v1/projects/Demo/eventarcTrigger?eventarcTriggerId=k", JSON, v2()),
    ("v2, an empty project", "POST", "/emulator/v1/projects//eventarcTrigger?eventarcTriggerId=k", JSON, v2()),
    ("v2, a database with a capital letter, a dot and an underscore", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="A_b.1")),
    ("v2, a database of a hyphen", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="-")),
    ("v2, a database of 100 characters", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="d" * 100)),
    ("v2, a database of 101 characters", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="d" * 101)),
    ("v2, a database with a space", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="a b")),
    ("v2, a database with a colon", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="a:b")),
    ("v2, a database of non-ASCII letters", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="é")),
    ("v2, (DEFAULT)", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="(DEFAULT)")),
    ("v2, (default) with a space", "POST", E + "?eventarcTriggerId=k", JSON, v2(database=" (default)")),
    ("v2, an empty database", "POST", E + "?eventarcTriggerId=k", JSON, v2(database="")),
    ("v2, no database", "POST", E + "?eventarcTriggerId=k", JSON, v2(database=...)),
    ("v2, another namespace", "POST", E + "?eventarcTriggerId=k", JSON, v2(namespace="other")),
    ("v2, an empty namespace", "POST", E + "?eventarcTriggerId=k", JSON, v2(namespace="")),
    ("v2, no namespace", "POST", E + "?eventarcTriggerId=k", JSON, v2(namespace=...)),

    # v1 bodies.
    ("v1, invalid JSON", "PUT", T + "k", JSON, "{"),
    ("v1, an empty body", "PUT", T + "k", JSON, ""),
    ("v1, a blank body", "PUT", T + "k", JSON, " "),
    ("v1, null", "PUT", T + "k", JSON, "null"),
    ("v1, an array", "PUT", T + "k", JSON, "[]"),
    ("v1, an empty object", "PUT", T + "k", JSON, "{}"),
    ("v1, an empty eventTrigger", "PUT", T + "k", JSON, '{"eventTrigger":{}}'),
    ("v1, eventTrigger null", "PUT", T + "k", JSON, '{"eventTrigger":null}'),
    ("v1, eventTrigger a string", "PUT", T + "k", JSON, '{"eventTrigger":"x"}'),
    ("v1, eventTrigger an array", "PUT", T + "k", JSON, '{"eventTrigger":[]}'),
    ("v1, an unknown field", "PUT", T + "k", JSON, compact({**json.loads(v1()), "foo": 1})),
    ("v1, an unknown field in eventTrigger", "PUT", T + "k", JSON, v1(foo=1)),
    ("v1, snake_case", "PUT", T + "k", JSON, compact({"event_trigger": {"event_type": V1_TYPES[0], "service": "firestore.googleapis.com"}})),
    ("v1, eventTrigger in both spellings", "PUT", T + "k", JSON, compact({**json.loads(v1()), "event_trigger": {}})),
    ("v1, eventType in both spellings", "PUT", T + "k", JSON, v1(event_type=V1_TYPES[0])),
    ("v1, a field twice in one spelling", "PUT", T + "k", JSON, '{"eventTrigger":{"eventType":"x","service":"firestore.googleapis.com","eventType":"' + V1_TYPES[0] + '"}}'),
    ("v1, no service", "PUT", T + "k", JSON, v1(service=...)),
    ("v1, another service", "PUT", T + "k", JSON, v1(service="pubsub.googleapis.com")),
    ("v1, the service in capitals", "PUT", T + "k", JSON, v1(service="FIRESTORE.googleapis.com")),
    ("v1, no eventType", "PUT", T + "k", JSON, v1(eventType=...)),
    ("v1, eventType null", "PUT", T + "k", JSON, v1(eventType=None)),
    ("v1, a v2 event type", "PUT", T + "k", JSON, v1(eventType=V2_TYPES[0])),
    ("v1, a Realtime Database event type", "PUT", T + "k", JSON, v1(eventType="providers/google.firebase.database/eventTypes/ref.write")),
    ("v1, an unknown event type and another service", "PUT", T + "k", JSON, v1(eventType="x", service="x")),
    ("v1, a number for a string", "PUT", T + "k", JSON, v1(resource=5)),
    ("v1, a boolean for a string", "PUT", T + "k", JSON, v1(resource=True)),
    ("v1, an array of one string for a string", "PUT", T + "k", JSON, v1(eventType=[V1_TYPES[0]])),
    ("v1, an array of two strings for a string", "PUT", T + "k", JSON, v1(eventType=[V1_TYPES[0], V1_TYPES[1]])),
    ("v1, an object for a string", "PUT", T + "k", JSON, v1(eventType={})),
    ("v1, a v2 body", "PUT", T + "k", JSON, v2()),

    # v2 bodies.
    ("v2, invalid JSON", "POST", E + "?eventarcTriggerId=k", JSON, "{"),
    ("v2, an empty body", "POST", E + "?eventarcTriggerId=k", JSON, ""),
    ("v2, null", "POST", E + "?eventarcTriggerId=k", JSON, "null"),
    ("v2, an empty object", "POST", E + "?eventarcTriggerId=k", JSON, "{}"),
    ("v2, an unknown field", "POST", E + "?eventarcTriggerId=k", JSON, v2(foo=1)),
    ("v2, an unknown field in document", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType="EXACT", foo=1)),
    ("v2, document a string", "POST", E + "?eventarcTriggerId=k", JSON, v2(document="items/{id}")),
    ("v2, no eventType", "POST", E + "?eventarcTriggerId=k", JSON, v2(eventType=...)),
    ("v2, a v1 event type", "POST", E + "?eventarcTriggerId=k", JSON, v2(eventType=V1_TYPES[0])),
    ("v2, an unknown event type", "POST", E + "?eventarcTriggerId=k", JSON, v2(eventType="google.cloud.firestore.document.v1.bogus")),
    ("v2, no document", "POST", E + "?eventarcTriggerId=k", JSON, v2(document=...)),
    ("v2, document null", "POST", E + "?eventarcTriggerId=k", JSON, v2(document=None)),
    ("v2, an empty document", "POST", E + "?eventarcTriggerId=k", JSON, doc()),
    ("v2, a document without matchType", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="items/{id}")),
    ("v2, a document without value", "POST", E + "?eventarcTriggerId=k", JSON, doc(matchType="EXACT")),
    ("v2, an empty value and matchType 0", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="", matchType=0)),
    ("v2, MATCH_TYPE_UNSPECIFIED", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType="MATCH_TYPE_UNSPECIFIED")),
    ("v2, matchType in lower case", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType="exact")),
    ("v2, matchType 1", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType=1)),
    ("v2, matchType \"2\"", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType="2")),
    ("v2, matchType 1.0", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType=1.0)),
    ("v2, matchType 3", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType=3)),
    ("v2, matchType 3 without value", "POST", E + "?eventarcTriggerId=k", JSON, doc(matchType=3)),
    ("v2, matchType -1 without value", "POST", E + "?eventarcTriggerId=k", JSON, doc(matchType=-1)),
    ("v2, matchType beyond 32 bits", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType=2147483648)),
    ("v2, matchType true", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType=True)),
    ("v2, matchType null", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType=None)),
    ("v2, matchType an array of one name", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="x", matchType=["EXACT"])),
    ("v2, snake_case", "POST", E + "?eventarcTriggerId=k", JSON, compact({"event_type": V2_TYPES[0], "document": {"value": "x", "match_type": "EXACT"}})),
    ("v2, eventType in both spellings", "POST", E + "?eventarcTriggerId=k", JSON, v2(event_type=V2_TYPES[0])),
    ("v2, fields in another order", "POST", E + "?eventarcTriggerId=k", JSON, compact({"document": {"matchType": "EXACT", "value": "items/a"}, "namespace": "(default)", "database": "(default)", "eventType": V2_TYPES[0]})),
    ("v2, a number and a boolean for strings", "POST", E + "?eventarcTriggerId=k", JSON, v2(database=1, document={"value": True, "matchType": "EXACT"})),
    ("v2, null strings", "POST", E + "?eventarcTriggerId=k", JSON, v2(database=None, namespace=None, document={"value": None, "matchType": "EXACT"})),
    ("v2, characters to escape", "POST", E + "?eventarcTriggerId=k", JSON, doc(value="\"\\/<'&=>\b\f\n\r\t\u0001\u001f\u007f  é😀", matchType="EXACT")),
    ("v2, a v1 body", "POST", E + "?eventarcTriggerId=k", JSON, v1()),

    # Methods, paths, content types, authorization.
    *[(f"v1, {m}", m, T + "us-central1-v1created-0", JSON, None) for m in ("GET", "DELETE", "HEAD")],
    ("v1, POST", "POST", T + "us-central1-v1created-0", JSON, v1()),
    ("v1, PATCH", "PATCH", T + "us-central1-v1created-0", JSON, v1()),
    *[(f"v2, {m}", m, E + "?eventarcTriggerId=us-central1-v2created-0", JSON, None) for m in ("GET", "DELETE")],
    ("v2, PUT", "PUT", E + "?eventarcTriggerId=k", JSON, v2()),
    ("v1, a trailing slash", "PUT", T + "k/", JSON, v1()),
    ("v1, no key", "PUT", T[:-1], JSON, v1()),
    ("v1, a key of two segments", "PUT", T + "a/b", JSON, v1()),
    ("v1, under a database", "PUT", f"/emulator/v1/projects/{P}/databases/(default)/triggers/k", JSON, v1()),
    ("v2, a trailing slash", "POST", E + "/?eventarcTriggerId=k", JSON, v2()),
    ("v2, the key in the path", "POST", E + "/k", JSON, v2()),
    ("v2, under a database", "POST", f"/emulator/v1/projects/{P}/databases/(default)/eventarcTrigger?eventarcTriggerId=k", JSON, v2()),
    ("v1, text/plain", "PUT", T + "k", {"Content-Type": "text/plain"}, v1()),
    ("v1, no content type", "PUT", T + "k", {}, v1()),
    ("v1, JSON with a charset", "PUT", T + "k", {"Content-Type": "application/json; charset=utf-8"}, v1()),
    ("v1, a form", "PUT", T + "k", {"Content-Type": "application/x-www-form-urlencoded"}, v1()),
    ("v1, multipart", "PUT", T + "k", {"Content-Type": "multipart/form-data"}, v1()),
    ("v2, no content type", "POST", E + "?eventarcTriggerId=k", {}, v2()),
    ("v2, a form", "POST", E + "?eventarcTriggerId=k", {"Content-Type": "application/x-www-form-urlencoded"}, v2()),
    ("v1, a mock token", "PUT", T + "k", {**JSON, "Authorization": "Bearer eyJhbGciOiJub25lIn0.eyJzdWIiOiJ1In0."}, v1()),
    ("v1, a token that is not a JWT", "PUT", T + "k", {**JSON, "Authorization": "Bearer garbage"}, v1()),
    ("v1, not a bearer token", "PUT", T + "k", {**JSON, "Authorization": "Basic x"}, v1()),
    ("v2, a token that is not a JWT", "POST", E + "?eventarcTriggerId=k", {**JSON, "Authorization": "Bearer garbage"}, v2()),
    ("v2, not a bearer token", "POST", E + "?eventarcTriggerId=k", {**JSON, "Authorization": "Basic x"}, v2()),
    # What is checked first.
    ("v1, invalid JSON and a bad token", "PUT", T + "k", {**JSON, "Authorization": "Bearer garbage"}, "{"),
    ("v1, invalid JSON and a bad project", "PUT", "/emulator/v1/projects/Demo/triggers/k", JSON, "{"),
    ("v1, a bad project and a bad token", "PUT", "/emulator/v1/projects/Demo/triggers/k", {**JSON, "Authorization": "Bearer garbage"}, v1()),
    ("v1, a blank key and a bad token", "PUT", T + "%20", {**JSON, "Authorization": "Bearer garbage"}, v1()),
    ("v1, a bad token and another service", "PUT", T + "k", {**JSON, "Authorization": "Bearer garbage"}, v1(service="x")),
    ("v2, eventarcTriggerId twice and a bad token", "POST", E + "?eventarcTriggerId=a&eventarcTriggerId=b", {**JSON, "Authorization": "Bearer garbage"}, v2()),
    ("v2, a bad project and a bad token", "POST", "/emulator/v1/projects/Demo/eventarcTrigger", {**JSON, "Authorization": "Bearer garbage"}, v2()),
    ("v2, a bad token and an unknown event type", "POST", E + "?eventarcTriggerId=k", {**JSON, "Authorization": "Bearer garbage"}, v2(eventType="x")),
]


def request(method, path, headers, body):
    conn = http.client.HTTPConnection(HOST, timeout=30)
    conn.request(method, path, body=None if body is None else body.encode(), headers=headers)
    res = conn.getresponse()
    text = res.read().decode()
    return {"status": res.status, "content_type": res.getheader("Content-Type"), "body": text}


def warnings(log, since):
    """The `WARNING` lines the jar printed after `since` bytes of its output, once it settled."""
    time.sleep(0.15)
    with open(log, "rb") as f:
        f.seek(since)
        printed = f.read().decode(errors="replace")
    return [line[len("WARNING: "):] for line in printed.splitlines() if line.startswith("WARNING: ")]


def run(flags, work):
    log = os.path.join(work, "emulator.out")
    with open(log, "wb") as out:
        proc = subprocess.Popen(
            [JAVA, "-Duser.language=en", "-Duser.country=US", "-jar", JAR, "--host", "127.0.0.1",
             "--port", str(PORT), *flags], cwd=work, stdout=out, stderr=subprocess.STDOUT)
    try:
        for _ in range(240):
            try:
                if request("GET", "/", {}, None)["status"] == 200:
                    break
            except OSError:
                time.sleep(0.25)
        else:
            raise SystemExit("the emulator did not start")
        cases = []
        for name, method, path, headers, body in CASES:
            since = os.path.getsize(log)
            outcome = request(method, path, headers, body)
            case = {"name": name, "method": method, "path": path, "headers": headers, "body": body, "outcome": outcome}
            printed = warnings(log, since)
            if printed:
                case["log"] = printed
            cases.append(case)
        return {"flags": flags, "cases": cases}
    finally:
        try:
            request("POST", "/shutdown", {}, None)
        except OSError:
            pass
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()


def main():
    with tempfile.TemporaryDirectory() as work:
        runs = [run([], work), run(FUNCTIONS, work)]
    fixture = {
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/function_triggers.py",
        "runs": runs,
    }
    compact_json = lambda v: json.dumps(v, ensure_ascii=False, separators=(",", ":"))
    print("{")
    for key in ("oracle", "generated_by"):
        print(f" {compact_json(key)}: {compact_json(fixture[key])},")
    print(' "runs": [')
    for i, r in enumerate(runs):
        print(f'  {{"flags": {compact_json(r["flags"])}, "cases": [')
        print(",\n".join("   " + compact_json(c) for c in r["cases"]))
        print("  ]}" + ("," if i + 1 < len(runs) else ""))
    print(" ]")
    print("}")


main()
