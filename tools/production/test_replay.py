"""Tests of tools/production/replay.py against a stand-in for production on 127.0.0.1."""

import json
import os
import sys
import tempfile
import threading
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import replay  # noqa: E402

PROJECT = "own-project"


class StandIn(BaseHTTPRequestHandler):
    """Answers as a database holding `documents`; records what it was sent."""

    documents = {}
    seen = []

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
            for write in json.loads(body)["writes"]:
                StandIn.documents.pop(write.get("delete"), None)
            return self.answer(200, {"commitTime": "2026-01-01T00:00:00Z"})
        if self.path.endswith(":runQuery"):
            link = f"https://console.firebase.google.com/v1/r/project/{PROJECT}/firestore/indexes?create_composite=Q2tS"
            return self.answer(400, [{"error": {"code": 400, "message": f"The query requires an index. You can create it here: {link}",
                                                "status": "FAILED_PRECONDITION"}}])
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
        self.assertEqual(replay.difference(official(404, '{"error":{"message":"Document (a) not found."}}'),
                                           official(404, '{"error": {"message": "Document \\"a\\" not found."}}')),
                         '$.error.message: "Document (a) not found.", production "Document \\"a\\" not found."')
        self.assertEqual(replay.difference(official(200, '[{"readTime":"<time>","done":true}]'),
                                           official(200, '[{"readTime":"2026-01-01T00:00:00Z"}]')),
                         "$[0]: keys only the official has ['done'], only production has []")
        self.assertEqual(replay.difference(official(200, '{"a":[1]}'), official(200, '{"a":[1,2]}')),
                         "$.a: 1 items, production 2")

    def test_a_query_that_needs_an_index_is_counted_apart(self):
        base = "/v1/projects/{project}/databases/(default)/documents"
        cases = [{"name": "ordered", "method": "POST", "path": base + "/c:runQuery", "headers": {"Authorization": "Bearer owner"},
                  "body": {"structuredQuery": {}}, "compare": "exact",
                  "outcome": {"status": 200, "contentType": "application/json", "body": "[]"}}]
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), self.fixture(cases))
        self.assertEqual((result["sent"], result["same"], result["differ"]), (1, 0, []))
        self.assertEqual(result["needsIndex"], ["ordered"])

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
