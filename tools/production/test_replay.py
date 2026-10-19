"""Tests of tools/production/replay.py against a stand-in for production on 127.0.0.1."""

import json
import os
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import replay  # noqa: E402
from test_indexes import index_link  # noqa: E402

PROJECT = "own-project"


class StandIn(BaseHTTPRequestHandler):
    """Answers as a database holding `documents`; records what it was sent."""

    documents = {}
    seen = []
    commits = 0

    def log_message(self, *args):
        pass

    def answer(self, status, body):
        data = json.dumps(body, indent=2).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=UTF-8")
        self.end_headers()
        self.wfile.write(data)

    def handle_one(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length).decode() if length else None
        StandIn.seen.append((self.command, self.path, self.headers.get("Authorization"), body))
        root = f"/v1/projects/{PROJECT}/databases/(default)/documents"
        if self.path == f"/v1/projects/{PROJECT}:test":
            # As the Rules API answers: `true` passes, `bad(` does not compile, `&& 1` warns.
            source = json.loads(body)["source"]["files"][0]["content"]
            if "bad(" in source:
                return self.answer(200, {"issues": [{"description": "mismatched input", "severity": "ERROR"}]})
            issues = [{"description": "Invalid type.", "severity": "WARNING"}] if "&& 1" in source else []
            state = "SUCCESS" if "if true;" in source else "FAILURE"
            return self.answer(200, {"issues": issues, "testResults": [{"state": state}]})
        if self.path.endswith(":listCollectionIds"):
            parent = urllib.parse.unquote(self.path[len("/v1/"):-len(":listCollectionIds")]) + "/"
            ids = sorted({n[len(parent):].split("/")[0] for n in StandIn.documents if n.startswith(parent)})
            return self.answer(200, {"collectionIds": ids} if ids else {})
        if self.command == "GET" and "showMissing=true" in self.path:
            # The collection's documents, and the missing ones that have documents below them.
            prefix = urllib.parse.unquote(self.path.split("?")[0][len("/v1/"):]) + "/"
            names = sorted({prefix + n[len(prefix):].split("/")[0] for n in StandIn.documents if n.startswith(prefix)})
            return self.answer(200, {"documents": [{"name": n} for n in names]} if names else {})
        if self.path == f"{root}:commit":
            if '"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx' in body:
                # As production refuses a document over 1 MiB, here one with a string of 50 characters.
                name = json.loads(body)["writes"][0]["update"]["name"]
                return self.answer(400, {"error": {"code": 400, "status": "INVALID_ARGUMENT", "message":
                    f"Document '{name}' cannot be written because its size (1,200,058 bytes) exceeds the maximum "
                    "allowed size of 1,048,576 bytes."}})
            StandIn.commits += 1
            now = f"2026-01-01T00:00:{StandIn.commits:02d}.000001Z"
            results = []
            for write in json.loads(body)["writes"]:
                if "update" in write:
                    StandIn.documents[write["update"]["name"]] = {"fields": write["update"].get("fields", {}), "time": now}
                    results.append({"updateTime": now})
                else:
                    results.append({})
                StandIn.documents.pop(write.get("delete"), None)
            return self.answer(200, {"writeResults": results, "commitTime": now})
        name = urllib.parse.unquote(self.path[len("/v1/"):])
        if self.command == "GET" and name in StandIn.documents:
            document = StandIn.documents[name]
            return self.answer(200, {"name": name, "fields": document["fields"], "createTime": document.get("time"),
                                     "updateTime": document.get("time")})
        if self.path.endswith((":runQuery", ":runAggregationQuery")):
            # As production answers: no `done`, an offset's count in a result of its own.
            parent, verb = urllib.parse.unquote(self.path[len("/v1/"):]).rsplit(":", 1)
            request = json.loads(body)
            query = request.get("structuredQuery") or request["structuredAggregationQuery"]["structuredQuery"]
            collection = query["from"][0]["collectionId"]
            if collection == "ix":
                link = index_link(PROJECT, "ix", [("a", 1), ("__name__", 1)])
                return self.answer(400, [{"error": {"code": 400, "message": f"The query requires an index. You can create it here: {link}",
                                                    "status": "FAILED_PRECONDITION"}}])
            names = sorted(n for n in StandIn.documents if n.rsplit("/", 1)[0] == f"{parent}/{collection}")
            now = "2026-01-01T00:00:00Z"
            if verb == "runAggregationQuery":
                return self.answer(200, [{"result": {"aggregateFields": {"n": {"integerValue": str(len(names))}}}, "readTime": now}])
            offset = query.get("offset", 0)
            results = [{"readTime": now, "skippedResults": offset}] if offset else []
            results += [{"document": {"name": n, "fields": StandIn.documents[n]["fields"], "createTime": now, "updateTime": now},
                         "readTime": now} for n in names[offset:]]
            return self.answer(200, results or [{"readTime": now}])
        if self.command == "PATCH":
            name = f"projects/{PROJECT}/databases/(default)/documents/" + self.path.split("/documents/")[1]
            StandIn.documents[name] = json.loads(body)
            return self.answer(200, {"name": name, "fields": json.loads(body).get("fields", {}),
                                     "createTime": "2026-01-01T00:00:00Z", "updateTime": "2026-01-01T00:00:00Z"})
        return self.answer(404, {"error": {"code": 404, "message": "not found", "status": "NOT_FOUND"}})

    do_GET = do_POST = do_PATCH = do_DELETE = handle_one


class ReplayTest(unittest.TestCase):
    def setUp(self):
        StandIn.documents = {f"projects/{PROJECT}/databases/(default)/documents/old/x": {}}
        StandIn.seen = []
        StandIn.commits = 0
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), StandIn)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.dir = tempfile.TemporaryDirectory()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.dir.cleanup()

    def fixture(self, cases):
        path = Path(self.dir.name) / "recording.json"
        path.write_text(json.dumps({"cases": cases}))
        return str(path)

    def test_cases_are_chosen_sent_and_compared(self):
        base = "/v1/projects/{project}/databases/(default)/documents"
        owner = {"Authorization": "Bearer owner"}
        written = {"status": 200, "contentType": "application/json",
                   "body": json.dumps({"name": "projects/{project}/databases/(default)/documents/c/a",
                                       "fields": {}, "createTime": "<time>", "updateTime": "<time>"}, indent=2)}
        cases = [
            {"name": "write", "method": "PATCH", "path": base + "/c/a", "headers": owner, "body": {"fields": {}},
             "compare": "exact", "outcome": written},
            {"name": "read a missing one", "method": "GET", "path": base + "/c/b", "headers": owner, "compare": "status",
             "outcome": {"status": 404, "contentType": "application/json", "body": "{}"}},
            {"name": "differs", "method": "GET", "path": base + "/c/z", "headers": owner, "compare": "exact",
             "outcome": {"status": 200, "contentType": "application/json", "body": "{}"}},
            {"name": "as a user", "method": "GET", "path": base + "/c/a", "headers": {"Authorization": "Bearer x"},
             "compare": "exact", "outcome": written},
            {"name": "another project", "method": "GET", "path": base + "/c/a", "headers": owner,
             "body": {"name": "projects/other/databases/(default)/documents/c/a"}, "compare": "exact", "outcome": written},
            {"name": "v1beta1", "method": "GET", "path": "/v1beta1/projects/{project}/databases/(default)/documents/c/a",
             "headers": owner, "compare": "exact", "outcome": written},
            {"name": "the emulator's own", "method": "DELETE", "path": "/emulator/v1/projects/{project}/databases/(default)/documents",
             "headers": owner, "compare": "exact", "outcome": written},
        ]
        production = replay.Production(self.base, PROJECT, "a-token", 100)
        result = replay.replay(production, self.fixture(cases))
        self.assertEqual((result["sent"], result["emulatorDeletes"]), (3, 1))
        self.assertEqual(result["same"], 2)
        self.assertEqual([d["name"] for d in result["differ"]], ["differs"])
        self.assertEqual(result["skipped"], {"not sent as the owner": 1, "names another project": 1,
                                             "not the (default) database's": 1})
        # The token replaces the owner's, the project its placeholder, and the database is
        # emptied before and after: the stand-in's document and the one written are gone.
        self.assertTrue(all(auth == "Bearer a-token" for _, _, auth, _ in StandIn.seen))
        self.assertIn(("PATCH", f"/v1/projects/{PROJECT}/databases/(default)/documents/c/a"),
                      [(m, p) for m, p, _, _ in StandIn.seen])
        self.assertEqual(StandIn.documents, {})

    def test_the_emulators_own_delete_empties_that_part_of_production(self):
        root = f"projects/{PROJECT}/databases/(default)/documents"
        StandIn.documents = {f"{root}/{name}": {} for name in ("ws/c", "ws/d", "ws/a/sub/x", "ws/a/other/y", "z/e")}
        production = replay.Production(self.base, PROJECT, "a-token", 100)
        # The emulator trims whitespace around each segment.
        case = {"method": "DELETE", "path": "/emulator/v1/projects/%20{project}/databases/(default)%09/documents/ws%20/c%E3%80%80"}
        production.empty(replay.emulator_delete(case))
        self.assertNotIn(f"{root}/ws/c", StandIn.documents)
        production.empty(replay.emulator_delete({"method": "DELETE", "path": case["path"].split("/documents")[0]
                                                 + "/documents/%20ws/a%20/sub"}))
        self.assertEqual(sorted(StandIn.documents), [f"{root}/ws/a/other/y", f"{root}/ws/d", f"{root}/z/e"])
        production.empty(replay.emulator_delete({"method": "DELETE",
                                                 "path": "/emulator/v1/projects/{project}%20/databases/(default)/documents"}))
        self.assertEqual(StandIn.documents, {})
        for other in ("/emulator/v1/projects/other/databases/(default)/documents",
                      "/emulator/v1/projects/{project}/databases/named/documents",
                      "/v1/projects/{project}/databases/(default)/documents/c/a"):
            self.assertIsNone(replay.emulator_delete({"method": "DELETE", "path": other}))
        self.assertIsNone(replay.emulator_delete({"method": "GET", "path": case["path"]}))

    def test_a_difference_is_located(self):
        def official(status, body):
            return {"status": status, "contentType": "application/json", "body": body}
        self.assertEqual(replay.difference(official(200, "{}"), official(400, "{}")), "status 200, production 400")
        self.assertEqual(replay.difference(official(404, '{"error":{"message":"Document (a) gone."}}'),
                                           official(404, '{"error": {"message": "No document a."}}')),
                         '$.error.message: "Document (a) gone.", production "No document a."')
        # A difference the parity exceptions record is not where they differ.
        self.assertEqual(replay.difference(official(404, '{"error":{"message":"Document (a) not found.", "x": 1}}'),
                                           official(404, '{"error": {"message": "Document \\"a\\" not found.", "x": 2}}')),
                         "$.error.x: 1, production 2")
        self.assertEqual(replay.difference(official(200, '[{"readTime":"<time>","done":true}]'),
                                           official(200, '[{"readTime":"2026-01-01T00:00:00Z", "x": 1}]')),
                         "$[0]: keys only the official has [], only production has ['x']")
        self.assertEqual(replay.difference(official(200, '{"a":[1]}'), official(200, '{"a":[1,2]}')),
                         "$.a: 1 items, production 2")

    def test_differences_the_exceptions_record_are_known(self):
        def known(official, production):
            found = set()
            return replay.same_json(official, production, found) and found
        error = {"error": {"code": 404, "message": "Document (p/c/d) not found.", "status": "NOT_FOUND"}}
        ours = {"error": {"code": 404, "message": 'Document "p/c/d" not found.', "status": "NOT_FOUND",
                          "details": [{"@type": "type.googleapis.com/google.rpc.BadRequest"}]}}
        self.assertEqual(known(error, ours), {"D097"})
        self.assertEqual(known(error, [ours]), {"D097", "D098"})
        self.assertEqual(known([{"readTime": "<time>", "done": True}], [{"readTime": "2026-01-01T00:00:00Z"}]), {"D099"})
        self.assertFalse(known(error, {"error": dict(ours["error"], status="INTERNAL")}))
        self.assertFalse(replay.same_json(error, ours))
        result = {"same": 0, "knownCases": 0, "known": {}, "differ": []}
        replay.tally(result, error, ours, lambda: "differs")
        replay.tally(result, error, error, lambda: "differs")
        replay.tally(result, error, {}, lambda: "differs")
        self.assertEqual(result, {"same": 1, "knownCases": 1, "known": {"D097": 1}, "differ": ["differs"]})
        # An error whose status differs too, and a case about a recorded difference.
        self.assertEqual(known({"status": "FAILED_PRECONDITION", "message": "Only a single array-contains clause is allowed in a query"},
                               {"status": "INVALID_ARGUMENT", "message": "A maximum of 1 'ARRAY_CONTAINS' filter is allowed per disjunction."}),
                         {"D106"})
        result = {"fixture": "rest.json", "same": 0, "knownCases": 0, "known": {}, "differ": []}
        replay.tally(result, {"status": 200}, {"status": 400}, lambda: "differs", 'JsonFormat: {"stringValue": 1}')
        replay.tally(result, {"status": 200}, {"status": 400}, lambda: "differs", "something new")
        self.assertEqual(result, {"fixture": "rest.json", "same": 0, "knownCases": 1, "known": {"D100": 1}, "differ": ["differs"]})

    def test_a_query_that_needs_an_index_is_counted_apart(self):
        base = "/v1/projects/{project}/databases/(default)/documents"
        cases = [{"name": "ordered", "method": "POST", "path": base + ":runQuery", "headers": {"Authorization": "Bearer owner"},
                  "body": {"structuredQuery": {"from": [{"collectionId": "ix"}]}}, "compare": "exact",
                  "outcome": {"status": 200, "contentType": "application/json", "body": "[]"}}]
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), self.fixture(cases))
        self.assertEqual((result["sent"], result["same"], result["differ"]), (1, 0, []))
        self.assertEqual(result["needsIndex"], ["ordered"])
        self.assertEqual(result["indexes"], [{
            "collectionGroup": "ix", "queryScope": "COLLECTION",
            "fields": [{"fieldPath": "a", "order": "ASCENDING"}, {"fieldPath": "__name__", "order": "ASCENDING"}]}])

    def test_a_recording_of_queries_is_seeded_run_and_compared(self):
        recording = {"dataset": [{"name": f"q/{x}", "fields": {"v": {"integerValue": "1"}}} for x in "abc"], "cases": [
            {"name": "all", "parent": "", "query": {"from": [{"collectionId": "q"}]},
             "outcome": {"status": "OK", "responses": [{"document": "q/a"}, {"document": "q/b"}, {"document": "q/c", "done": True}]}},
            {"name": "offset 1", "parent": "", "query": {"from": [{"collectionId": "q"}], "offset": 1},
             "outcome": {"status": "OK", "responses": [{"document": "q/b", "skipped": 1}, {"document": "q/c", "skipped": 1, "done": True}]}},
            {"name": "a projection", "parent": "", "query": {"from": [{"collectionId": "q"}], "select": {"fields": []}, "offset": 2},
             "outcome": {"status": "OK", "responses": [{"document": "q/c", "skipped": 2, "done": True}],
                         "fields": {"q/c": {"v": {"integerValue": "1"}}}}},
            {"name": "differs", "parent": "", "query": {"from": [{"collectionId": "q"}]},
             "outcome": {"status": "OK", "responses": [{"document": "q/a", "done": True}]}},
            {"name": "needs an index", "parent": "", "query": {"from": [{"collectionId": "ix"}]},
             "outcome": {"status": "OK", "responses": [{"done": True}]}},
            {"name": "count", "parent": "", "query": {"from": [{"collectionId": "q"}]}, "aggregations": [{"count": {}, "alias": "n"}],
             "outcome": {"status": "OK", "responses": [{"result": {"aggregateFields": {"n": {"integerValue": "3"}}},
                                                        "readTime": "<time>", "done": True}]}},
            {"name": "another project", "parent": "", "query": {"from": [{"collectionId": "q"}], "where": {"fieldFilter": {
                "field": {"fieldPath": "__name__"}, "op": "EQUAL",
                "value": {"referenceValue": "projects/other/databases/(default)/documents/q/a"}}}},
             "outcome": {"status": "OK", "responses": [{"done": True}]}},
        ]}
        path = Path(self.dir.name) / "queries.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["sent"], result["same"]), (6, 4))
        self.assertEqual([d["name"] for d in result["differ"]], ["differs"])
        self.assertEqual(result["differ"][0]["difference"], "$.documents: 1 items, production 3")
        self.assertEqual(result["needsIndex"], ["needs an index"])
        self.assertEqual(result["skipped"], {"names another project": 1})
        self.assertEqual(StandIn.documents, {})
        # A case about a recorded difference, by its name, is known.
        replay.KNOWN_CASES["queries.json"].append(("D000", r"^differs$"))
        self.addCleanup(replay.KNOWN_CASES["queries.json"].pop)
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["differ"], result["known"]), ([], {"D000": 1}))

    def test_a_recording_of_limits_commits_each_document(self):
        recording = {"cases": [
            {"name": "small", "path": ["c", ["repeat", "d", 3]], "fields": {"s": ["string", 2, "\u20ac"], "f": ["nest", 3, True]},
             "outcome": {"status": 200, "message": ""}},
            {"name": "too big", "path": ["c", "x"], "fields": {"s": ["string", 60, "x"]},
             "outcome": {"status": 400, "message": "maximum entity size is 1048576 bytes"}},
            {"name": "differs", "path": ["c", "x"], "fields": {"b": ["bytes", 3], "m": ["map", {"a": ["array", [["int"]]]}]},
             "outcome": {"status": 400, "message": "too long"}},
        ]}
        path = Path(self.dir.name) / "limits.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["sent"], result["same"], result["known"]), (3, 1, {"D075": 1}))
        self.assertEqual([(d["name"], d["difference"]) for d in result["differ"]], [("differs", "$.status: 400, production 200")])
        self.assertNotIn(PROJECT, json.dumps(result))
        first = next(w["update"] for method, path, _, body in StandIn.seen if path.endswith(":commit")
                     for w in json.loads(body)["writes"] if "update" in w)
        self.assertEqual(first, {"name": f"projects/{PROJECT}/databases/(default)/documents/c/ddd", "fields": {
            "s": {"stringValue": "\u20ac\u20ac"},
            "f": {"arrayValue": {"values": [{"mapValue": {"fields": {"m": {"arrayValue": {"values": [{"integerValue": "1"}]}}}}}]}}}})
        self.assertEqual(replay.limit_value(["bytes", 3]), {"bytesValue": "eHh4"})
        self.assertEqual(StandIn.documents, {})

    def test_a_pace_spaces_the_requests(self):
        pace, started = replay.Pace(1200), time.monotonic()
        for _ in range(5):
            pace.wait()
        self.assertGreaterEqual(time.monotonic() - started, 0.2)

    def test_rules_are_tested_with_the_rules_api(self):
        template = "service cloud.firestore {\n  match /rooms/{room} {\n    allow read: if %s;\n  }\n}\n"
        recording = {"document": {}, "rules": template, "results": [
            {"name": "true", "condition": "true", "outcome": "allow"},
            {"name": "a warning", "condition": "false && 1", "issues": ["Invalid type."], "outcome": "deny", "message": "…"},
            {"name": "does not compile", "condition": "bad(", "outcome": "compile", "message": "…"},
            {"name": "differs", "condition": "false", "outcome": "allow"},
            {"name": "a whole ruleset", "rules": template.replace("%s", "true"), "outcome": "allow"},
            {"name": "reads the document", "condition": "resource.data.n is int", "outcome": "allow"},
            {"name": "a batch", "condition": "true", "writes": [], "outcome": "allow"},
        ]}
        path = Path(self.dir.name) / "rules_eval.json"
        path.write_text(json.dumps(recording))
        production = replay.Production(self.base, PROJECT, "a-token", 100, rules=self.base)
        result = replay.replay(production, str(path))
        self.assertEqual((result["sent"], result["same"]), (5, 4))
        self.assertEqual([d["name"] for d in result["differ"]], ["differs"])
        self.assertEqual(result["differ"][0]["difference"], '$.outcome: "allow", production "deny"')
        self.assertEqual(result["skipped"], {"reads the document, the request or other documents": 1,
                                             "a batch of writes": 1})

    def test_a_recording_of_steps_runs_each_case_on_an_empty_database(self):
        def step(method, path, status, response, body=None):
            return {"method": method, "path": path, "body": body, "status": status, "response": response}
        d = "projects/s-1/databases/(default)/documents"
        written = "2026-10-13T14:35:37.991178Z"
        recording = {"cases": [
            {"name": "write then read", "steps": [
                step("POST", f"{d}:commit", 200, {"writeResults": [{"updateTime": written}], "commitTime": written},
                     {"writes": [{"update": {"name": f"{d}/c/d", "fields": {"v": {"integerValue": "1"}}}}]}),
                step("GET", f"{d}/c/d", 200, {"name": f"{d}/c/d", "fields": {"v": {"integerValue": "1"}},
                                              "createTime": written, "updateTime": written})]},
            {"name": "a missing document", "steps": [
                step("GET", "projects/s-2/databases/(default)/documents/c/d", 404,
                     {"error": {"code": 404, "message": "not found", "status": "NOT_FOUND"}})]},
            {"name": "differs", "steps": [step("GET", "projects/s-3/databases/(default)/documents/c/x", 200, {})]},
            {"name": "a status alone", "steps": [{"method": "POST", "path": "/v1/projects/s-4/databases/(default)/documents:commit",
                                                  "body": {"writes": []}, "owner": True, "status": 200}]},
            {"name": "with rules", "rules": True, "steps": [dict(step("GET", f"{d}/c/d", 403, {}), owner=False)]},
        ]}
        path = Path(self.dir.name) / "same_document_writes.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["sent"], result["same"]), (4, 3))
        self.assertEqual([d["name"] for d in result["differ"]], ["differs, step 1"])
        self.assertEqual(result["skipped"], {"evaluates rules": 1})

    def test_times_sent_follow_the_commits(self):
        official, production = ["2026-10-10T02:41:04.131568Z"], ["2026-10-18T05:00:00.000010Z"]
        path = "c/d?readTime=2026-10-10T02%3A41%3A04.131567Z&x=2026-10-10T03%3A41%3A04.131568Z&y=2001-01-01T00:00:00Z"
        self.assertEqual(replay.shift(path, official, production),
                         "c/d?readTime=2026-10-18T05%3A00%3A00.000009Z&x=2026-10-18T06%3A00%3A00.000010Z&y=2001-01-01T00:00:00Z")
        body = '{"currentDocument": {"updateTime": "2026-10-10T02:41:04.131568Z"}}'
        self.assertEqual(replay.shift(body, official, production), '{"currentDocument": {"updateTime": "2026-10-18T05:00:00.000010Z"}}')
        self.assertEqual(replay.shift(body, official, []), body)

    def test_what_an_answer_hands_out_is_carried(self):
        found = {}
        replay.carried({"documents": [], "nextPageToken": "Cj/x="}, {"documents": [], "nextPageToken": "AFT"}, found)
        replay.carried([{"transaction": "T1"}], [{"transaction": "P1"}], found)
        self.assertEqual(found, {"Cj/x=": "AFT", "T1": "P1"})
        self.assertEqual(replay.handed_on("c?pageToken=Cj%2Fx%3D", found), "c?pageToken=AFT")
        self.assertEqual(replay.handed_on('{"transaction": "T1"}', found), '{"transaction": "P1"}')

    def test_templates_are_filled_with_the_commits(self):
        commits = ["2026-10-18T05:00:00.000010Z", "2026-10-18T05:00:01Z"]
        self.assertEqual(replay.filled('{"updateTime": "{C1+1}"} {C2} {C3}', commits),
                         '{"updateTime": "2026-10-18T05:00:00.000011Z"} 2026-10-18T05:00:01.000000Z {C3}')
        self.assertEqual(replay.filled("the stored version ({V1}) does not match", commits),
                         "the stored version (1792299600000010) does not match")

    def test_times_are_read_against_the_commits(self):
        commits = []
        answer = {"writeResults": [{"updateTime": "2026-01-01T00:00:01.5Z", "transformResults": [
            {"timestampValue": "2026-01-01T00:00:01.2Z"}, {"timestampValue": "2020-01-01T00:00:00Z"}]}],
            "commitTime": "2026-01-01T00:00:01.5Z"}
        self.assertEqual(replay.step_answer(200, answer, "p", commits)["body"], {
            "writeResults": [{"updateTime": "<commit 1>", "transformResults": [
                {"timestampValue": "<near commit 1>"}, {"timestampValue": "2020-01-01T00:00:00Z"}]}],
            "commitTime": "<commit 1>"})
        self.assertEqual(replay.step_answer(200, {"readTime": "2027-01-01T00:00:00Z"}, "p", commits)["body"],
                         {"readTime": "<time>"})
        missing = {"error": {"status": "NOT_FOUND", "message": "The database second does not exist for project own-1 Please visit "
                                                          "https://console.cloud.google.com/datastore/setup?project=own-1"}}
        self.assertNotIn("own-1", json.dumps(replay.step_answer(404, missing, "own-1", [])))
        listed = {"documents": [{"name": "projects/p/databases/(default)/documents/c/TCvDCsQgNWUpqCrYXQTB"}], "nextPageToken": "x"}
        self.assertEqual(replay.step_answer(200, listed, "p", [])["body"],
                         {"documents": [{"name": "projects/{project}/databases/(default)/documents/c/<auto>"}], "nextPageToken": "<token>"})
        self.assertEqual(replay.step_answer(400, {"error": {"status": "FAILED_PRECONDITION", "message":
                                                            "the stored version (1792261663729293) does not match"}}, "p", [])["message"],
                         "the stored version (<version>) does not match")

    def test_a_placeholder_matches_within_a_string(self):
        official = '{"error":{"message":"the stored version (<version>) does not match (42)"}}'
        self.assertTrue(replay.same_body(official, '{\n  "error": {"message": "the stored version (1792) does not match (42)"}\n}'))
        self.assertFalse(replay.same_body(official, '{"error": {"message": "the stored version (1792) does not match (43)"}}'))
        self.assertFalse(replay.same_body(official, '{"error": {"message": "the stored version () does not match (42)"}}'))
        self.assertFalse(replay.same_body('{"a": "<time>"}', '{"a": 1}'))
        self.assertTrue(replay.same_body('{"a": "x.y"}', '{"a": "x.y"}'))
        # An index into a resource name counts the project ID's characters.
        self.assertTrue(replay.same_body('{"m": "Document name \\"x\\" lacks \\"/\\" at index 53."}',
                                         '{"m": "Document name \\"x\\" lacks \\"/\\" at index 52."}'))
        self.assertFalse(replay.same_body('{"m": "lacks \\"/\\" at index 53."}', '{"m": "has \\"/\\" at index 52."}'))
        # Numbers by value; a boolean is not a number.
        self.assertTrue(replay.same_body('{"doubleValue": 1.0}', '{"doubleValue": 1}'))
        self.assertFalse(replay.same_body('{"doubleValue": 1.5}', '{"doubleValue": 1}'))
        self.assertFalse(replay.same_body('{"a": true}', '{"a": 1}'))
        self.assertFalse(replay.same_body('{"a": "x.y"}', '{"a": "xzy"}'))

    def test_the_budget_stops_a_run_and_the_report_is_written(self):
        base = "/v1/projects/{project}/databases/(default)/documents"
        cases = [{"name": f"read {i}", "method": "GET", "path": f"{base}/c/{i}", "headers": {"Authorization": "Bearer owner"},
                  "compare": "status", "outcome": {"status": 404, "contentType": "application/json", "body": "{}"}}
                 for i in range(20)]
        out = Path(self.dir.name) / "out"
        os.environ[replay.TOKEN_ENV] = "a-token"
        try:
            with self.assertRaisesRegex(RuntimeError, "budget of 10"):
                replay.main(["--project", PROJECT, "--out", str(out), "--base", self.base, "--budget", "10",
                             self.fixture(cases)])
        finally:
            del os.environ[replay.TOKEN_ENV]
        report = json.loads((out / "report.json").read_text())
        self.assertEqual(report["requests"], 10)
        self.assertEqual(report["project"], "{project}")
        self.assertNotIn("a-token", (out / "report.json").read_text() + (out / "report.md").read_text())


if __name__ == "__main__":
    unittest.main()
