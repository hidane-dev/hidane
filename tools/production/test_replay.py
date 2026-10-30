"""Tests of tools/production/replay.py against a stand-in for production on 127.0.0.1."""

import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import release  # noqa: E402
import replay  # noqa: E402
from test_indexes import index_link  # noqa: E402

PROJECT = "own-project"


class StandIn(BaseHTTPRequestHandler):
    """Answers as a database holding `documents`; records what it was sent."""

    documents = {}
    seen = []
    commits = 0
    targets = []
    listened = []
    # The Rules API's rulesets (name: content), the project's release (a ruleset's name), what it
    # released, in order, and how many requests after a release the one before still decides.
    rulesets = {}
    release = None
    released = []
    lag = 0
    lagging = 0

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
        if self.path.startswith("/google.firestore.v1.Firestore/Listen/channel"):
            return self.channel(body)
        if self.path == f"/v1/projects/{PROJECT}:test":
            return self.rules_api(json.loads(body))
        if self.path.startswith((f"/v1/projects/{PROJECT}/rulesets", f"/v1/projects/{PROJECT}/releases")):
            return self.deployments(body)
        if self.headers.get("Authorization") is None and not self.decided():
            return self.answer(403, {"error": {"code": 403, "message": "Missing or insufficient permissions.",
                                               "status": "PERMISSION_DENIED"}})
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
            if '"arrayValue": {"values": [{"arrayValue"' in body:
                return self.answer(400, {"error": {"code": 400, "message": "Nested arrays are not allowed",
                                                   "status": "INVALID_ARGUMENT"}})
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
        path = urllib.parse.unquote(self.path.split("?")[0][len("/v1/"):])
        below = path.split("/documents/", 1)[1].split("/") if "/documents/" in path else []
        if self.command == "GET" and "showMissing" not in self.path and (path.endswith("/") or len(below) % 2):
            # ListDocuments: a collection's documents, or without a collection ID (a path that ends
            # with a slash) those right below the parent, by name.
            prefix, depth = (path, 1) if path.endswith("/") else (path + "/", 0)
            query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)
            names = sorted(n for n in StandIn.documents if n.startswith(prefix) and n[len(prefix):].count("/") == depth)
            size, start = int(query.get("pageSize", ["0"])[0]), int(query.get("pageToken", ["0"])[0])
            page = names[start:start + size] if size else names[start:]
            listing = {"documents": [{"name": n, "fields": StandIn.documents[n]["fields"]} for n in page]} if page else {}
            if size and start + size < len(names):
                listing["nextPageToken"] = str(start + size)
            return self.answer(200, listing)
        name = urllib.parse.unquote(self.path.split("?")[0][len("/v1/"):])
        if self.command == "GET" and name in StandIn.documents:
            document = StandIn.documents[name]
            return self.answer(200, {"name": name, "fields": document["fields"], "createTime": document.get("time"),
                                     "updateTime": document.get("time")})
        if self.path.endswith((":runQuery", ":runAggregationQuery")):
            # As production answers: no `done`, an offset's count in a result of its own.
            parent, verb = urllib.parse.unquote(self.path[len("/v1/"):]).rsplit(":", 1)
            # As proto3 JSON, field names in either spelling: `structured_query` or `structuredQuery`.
            request = json.loads(re.sub(r'"(\w+)":', lambda m: '"%s":' % re.sub(r"_(\w)", lambda c: c.group(1).upper(), m.group(1)), body))
            query = request.get("structuredQuery") or request["structuredAggregationQuery"]["structuredQuery"]
            if "collectionId" not in query["from"][0]:
                # Every collection below the parent, by name, as the replay's emptying asks.
                after = query.get("startAt", {}).get("values", [{}])[0].get("referenceValue", "")
                names = sorted(n for n in StandIn.documents if n.startswith(parent + "/") and n > after)
                return self.answer(200, [{"document": {"name": n}, "readTime": "2026-01-01T00:00:00Z"}
                                         for n in names[:query["limit"]]] or [{"readTime": "2026-01-01T00:00:00Z"}])
            collection = query["from"][0]["collectionId"]
            if collection == "ix":
                link = index_link(PROJECT, "ix", [("a", 1), ("__name__", 1)])
                return self.answer(400, [{"error": {"code": 400, "message": f"The query requires an index. You can create it here: {link}",
                                                    "status": "FAILED_PRECONDITION"}}])
            if query["from"][0].get("allDescendants"):
                names = sorted(n for n in StandIn.documents if n.split("/")[-2] == collection)
            else:
                names = sorted(n for n in StandIn.documents if n.rsplit("/", 1)[0] == f"{parent}/{collection}")
            if query.get("orderBy"):
                names.sort(key=lambda n: int(StandIn.documents[n]["fields"]["v"]["integerValue"]))
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

    def rules_api(self, request):
        """As the Rules API answers `projects.test`: `bad(` does not compile, `foo()` and `&& 1`
        warn; a test passes for `true`, or for a document whose ID the ruleset names (`'x'`), and
        fails where the ruleset calls `debug()`, which the API does not have. It takes one file,
        which is not empty, and refuses fields it does not know."""
        def refused(message, details=False):
            error = {"code": 400, "message": message, "status": "INVALID_ARGUMENT"}
            if details:
                error["details"] = [{"@type": "type.googleapis.com/google.rpc.BadRequest"}]
            return self.answer(400, {"error": error})
        unknown = sorted(request.keys() - {"source", "testSuite"})
        if unknown:
            return refused(f'Invalid JSON payload received. Unknown name "{unknown[0]}": Cannot find field.', True)
        files = request["source"]["files"]
        if len(files) > 1:
            return refused("Currently only one file source can be passed to firebase rules.")
        if not files[0].get("content"):
            return refused("Content must not be blank.")
        source = files[0]["content"]
        at = {"fileName": files[0]["name"], "line": 5, "column": 22, "currentOffset": 137, "endOffset": 139}
        issues = [{"sourcePosition": at, "description": "mismatched input", "severity": "ERROR"}] if "bad(" in source else []
        issues += [{"sourcePosition": at, "description": "Invalid function name: foo.", "severity": "WARNING"}] if "foo()" in source else []
        issues += [{"description": "Invalid type.", "severity": "WARNING"}] if "&& 1" in source else []
        answer = {"issues": issues} if issues else {}
        if any(i["severity"] == "ERROR" for i in issues):
            return self.answer(200, answer)
        cases = request.get("testSuite", {}).get("testCases", [])
        if cases and "debug(" in source:
            answer["testResults"] = [{"state": "FAILURE", "debugMessages": [
                "Error: firestore.rules line [5], column [22]. Function not found error: Name: [debug]."]} for _ in cases]
        elif cases:
            answer["testResults"] = [{"state": "SUCCESS" if "if true;" in source or "'" + c["request"]["path"].rsplit("/", 1)[1] + "'" in source
                                      else "FAILURE"} for c in cases]
        return self.answer(200, answer)

    def deployments(self, body):
        """As the Rules API keeps rulesets and the project's release: a ruleset that does not
        compile (`bad(`) is refused, and one the release uses is not deleted."""
        kind, _, rest = self.path.split("?")[0][len(f"/v1/projects/{PROJECT}/"):].partition("/")
        missing = {"error": {"code": 404, "message": "not found", "status": "NOT_FOUND"}}
        if kind == "rulesets" and self.command == "POST":
            content = json.loads(body)["source"]["files"][0]["content"]
            if "bad(" in content:
                return self.answer(400, {"error": {"code": 400, "message": "Syntax error", "status": "INVALID_ARGUMENT"}})
            name = f"projects/{PROJECT}/rulesets/made-{len(StandIn.released)}-{len(StandIn.rulesets)}-{time.monotonic_ns()}"
            StandIn.rulesets[name] = content
            return self.answer(200, {"name": name, "createTime": "2026-01-01T00:00:00Z"})
        if kind == "rulesets" and not rest:
            return self.answer(200, {"rulesets": [{"name": n} for n in sorted(StandIn.rulesets)]})
        if kind == "rulesets":
            name = f"projects/{PROJECT}/rulesets/{rest}"
            if name not in StandIn.rulesets:
                return self.answer(404, missing)
            if self.command == "DELETE":
                if StandIn.release == name:
                    return self.answer(400, {"error": {"code": 400, "message": "in use", "status": "FAILED_PRECONDITION"}})
                del StandIn.rulesets[name]
                return self.answer(200, {})
            return self.answer(200, {"name": name, "source": {"files": [{"name": "firestore.rules", "content": StandIn.rulesets[name]}]}})
        if self.command in ("GET", "DELETE") and StandIn.release is None:
            return self.answer(404, missing)
        if self.command == "DELETE":
            StandIn.release = None
            return self.answer(200, {})
        if self.command == "POST" and StandIn.release is not None:
            return self.answer(409, {"error": {"code": 409, "message": "exists", "status": "ALREADY_EXISTS"}})
        if self.command == "PATCH" and StandIn.release is None:
            return self.answer(404, missing)
        if self.command in ("POST", "PATCH"):
            request = json.loads(body)
            StandIn.release = request.get("release", request)["rulesetName"]
            StandIn.released.append(StandIn.rulesets[StandIn.release])
            StandIn.lagging = StandIn.lag
        return self.answer(200, {"name": f"projects/{PROJECT}/releases/cloud.firestore", "rulesetName": StandIn.release})

    def decided(self):
        """Whether the rules released allow a request without credentials. The release before
        decides the first `lag` requests after each release, which has not reached every server.
        A rule is `allow <methods>: if <condition>` in `match /<collection>/{id}` or
        `match /{document=**}`, and a condition `true`, `false` or `resource.data.n == <n>`."""
        source = StandIn.rulesets.get(StandIn.release)
        if StandIn.lagging:
            StandIn.lagging -= 1
            source = StandIn.released[-2] if len(StandIn.released) > 1 else None
        if source is None:
            return False
        name = urllib.parse.unquote(self.path.split("?")[0][len("/v1/"):])
        if "/documents/" not in name:
            return False
        below = name.split("/documents/", 1)[1].split("/")
        document = StandIn.documents.get(name)
        op = {"GET": "get" if len(below) % 2 == 0 else "list", "DELETE": "delete"}.get(
            self.command, "update" if document else "create")
        for where, allows in re.findall(r"match /(\{document=\*\*\}|\w+)(?:/\{\w+\})? \{\s*((?:allow [^;]+;\s*)+)\}", source):
            if where not in ("{document=**}", below[0]):
                continue
            for methods, condition in re.findall(r"allow ([\w, ]+): if ([^;]+);", allows):
                methods = {m.strip() for m in methods.split(",")}
                if op not in methods and not ("read" in methods and op in ("get", "list")) and \
                        not ("write" in methods and op in ("create", "update", "delete")):
                    continue
                n = re.fullmatch(r"resource\.data\.n == (\d+)", condition.strip())
                if condition.strip() == "true" or (n and document and
                                                   document["fields"].get("n", {}).get("integerValue") == n.group(1)):
                    return True
        return False

    def channel(self, body):
        """A WebChannel Listen stream, as production serves one: a session for the first request's
        target, then the stream, in frames: the target's documents by name, then `CURRENT`; or its
        removal (a collection `ix` needs an index, and one named `sel` stands for a projection)."""
        query = urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)

        def frames(chunks):
            text = json.dumps([[n, chunk] for n, chunk in enumerate(chunks)])
            data = f"{len(text)}\n{text}".encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("X-HTTP-Session-Id", "a-session")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        if "SID" not in query:
            form = urllib.parse.parse_qs(body)
            StandIn.listened.append(form["headers"][0])
            StandIn.targets.append(json.loads(form["req0___data__"][0])["addTarget"])
            return frames([["c", f"session-{len(StandIn.targets)}", "", 8, 15, 30000]])
        if query.get("TYPE") == ["terminate"]:
            return frames([])
        assert query.get("gsessionid") == ["a-session"]
        target = StandIn.targets[int(query["SID"][0].split("-")[1]) - 1]["query"]
        collection = target["structuredQuery"]["from"][0]["collectionId"]
        if collection in ("ix", "sel"):
            message = ("'select' clauses are not supported for real-time queries." if collection == "sel" else
                       "The query requires an index. You can create it here: " + index_link(PROJECT, "ix", [("a", 1), ("__name__", 1)]))
            return frames([[{"targetChange": {"targetChangeType": "REMOVE", "targetIds": [2], "cause": {"code": 9, "message": message}}}]])
        names = sorted(n for n in StandIn.documents if n.rsplit("/", 1)[0] == f"{target['parent']}/{collection}")
        return frames([[{"targetChange": {"targetChangeType": "ADD", "targetIds": [2]}}]]
                      + [[{"documentChange": {"document": {"name": n}, "targetIds": [2]}}] for n in names]
                      + [[{"targetChange": {"targetChangeType": "CURRENT", "targetIds": [2]}}]])

    do_GET = do_POST = do_PATCH = do_DELETE = handle_one


class ReplayTest(unittest.TestCase):
    def setUp(self):
        StandIn.documents = {f"projects/{PROJECT}/databases/(default)/documents/old/x": {}}
        StandIn.seen = []
        StandIn.commits = 0
        StandIn.targets, StandIn.listened = [], []
        StandIn.rulesets, StandIn.release, StandIn.released, StandIn.lag, StandIn.lagging = {}, None, [], 0, 0
        self.waits = release.SETTLE, replay.RETRY_AFTER, release.PER_MINUTE
        release.SETTLE = replay.RETRY_AFTER = 0
        release.PER_MINUTE = 10 ** 9
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), StandIn)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.dir = tempfile.TemporaryDirectory()

    def tearDown(self):
        release.SETTLE, replay.RETRY_AFTER, release.PER_MINUTE = self.waits
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
        self.assertEqual(known({"status": 403, "error": "PERMISSION_DENIED", "message": "No matching allow statements"},
                               {"status": 403, "error": "PERMISSION_DENIED", "message": "Missing or insufficient permissions."}),
                         {"D118"})
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

    def test_the_order_of_values_and_names_is_redone(self):
        def item(label, n):
            return {"label": label, "value": {"integerValue": str(n)}}
        recording = {"values": {"groups": [[item("1", 1)], [item("2a", 2), item("2b", 2)], [item("3", 3)]], "rejected": [
            {"label": "[[]]", "value": {"arrayValue": {"values": [{"arrayValue": {}}]}}, "error": "Nested arrays are not allowed"}]},
            "paths": {"order": ["c/a/items/x", "items/top"], "rejected": []}}
        path = Path(self.dir.name) / "value_order.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["cases"], result["sent"], result["same"], result["differ"]), (4, 4, 4, []))
        self.assertEqual(StandIn.documents, {})
        # Values the official emulator held apart, where production finds them equal, differ.
        recording["values"]["groups"][1:2] = [[item("2a", 2)], [item("2b", 2)]]
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual([(d["name"], d["difference"]) for d in result["differ"]], [
            ("values: groups of equal values, in order", "$: 4 items, production 3")])

    def test_list_documents_is_replayed_over_rest(self):
        both = ["m", "n"]
        recording = {"seed": [["c/a", "d/b"], ["c/b"]], "grpc": [
            {"name": "a collection", "request": {"collection_id": "c"}, "parent": "", "owner": True,
             "outcome": {"documents": [{"name": "c/a", "fields": both}, {"name": "c/b", "fields": both}]}},
            {"name": "a collection, anonymous", "request": {"collection_id": "c"}, "parent": "", "owner": False,
             "outcome": {"code": "PERMISSION_DENIED", "message": "denied"}},
            {"name": "every collection", "request": {}, "parent": "", "owner": True, "outcome": {"documents": []}},
            {"name": "pages of 1", "request": {"collection_id": "c", "page_size": 1, "pages": True}, "parent": "",
             "owner": True, "outcome": {"pages": [["c/a"], ["c/b"]]}},
            {"name": "differs", "request": {"collection_id": "c", "page_size": 1}, "parent": "", "owner": True,
             "outcome": {"documents": []}},
        ], "rest": [
            {"name": "root with a slash", "path": "/", "outcome": {"status": 200, "documents": ["c/a", "c/b", "d/b"],
                                                                   "next_page": False}},
        ]}
        path = Path(self.dir.name) / "list_documents.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["cases"], result["sent"], result["same"]), (6, 4, 3))
        self.assertEqual(result["skipped"], {"not sent as the owner": 1, "lists every collection, which REST cannot ask": 1})
        self.assertEqual([(d["name"], d["difference"]) for d in result["differ"]], [("differs", "$.documents: 0 items, production 1")])
        sent = [p for m, p, _, _ in StandIn.seen if m == "GET"]
        self.assertIn(f"/v1/projects/{PROJECT}/databases/(default)/documents/c?pageSize=1&pageToken=1", sent)
        self.assertEqual(StandIn.documents, {})

    def test_reads_at_a_read_time_go_to_a_collection_of_their_own(self):
        documents = "projects/read-time/databases/(default)/documents"
        recording = {"databases": {}, "read_time": {"reads": [
            {"name": "GetDocument", "transport": "grpc", "method": "GetDocument",
             "body": {"name": f"{documents}/c/d", "readTime": "{readTime}"}},
            {"name": "elsewhere", "transport": "grpc", "method": "GetDocument",
             "body": {"name": "projects/read-time-empty/databases/(default)/documents/c/d", "readTime": "{readTime}"}},
            {"name": "REST :runQuery", "transport": "rest", "method": "POST", "path": f"/v1/{documents}:runQuery",
             "body": {"structuredQuery": {"from": [{"collectionId": "c"}]}, "readTime": "{readTime}"}},
        ], "cases": [{"age": 0, "answers": [
            {"status": "OK", "message": "", "documents": ["c/d"]},
            {"status": "NOT_FOUND", "message": "not found"},
            {"status": 200, "response": [{"document": {"name": f"{documents}/c/d", "fields": {"a": {"integerValue": "1"}},
                                                       "createTime": "<time>", "updateTime": "<time>"},
                                          "readTime": "<time>", "done": True}]},
        ]}]}}
        path = Path(self.dir.name) / "read_time_and_databases.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["cases"], result["sent"], result["same"], result["known"], result["differ"]),
                         (3, 2, 1, {"D099": 1}, []))
        self.assertEqual(result["skipped"], {"names another project": 1})
        read = next(p for m, p, _, _ in StandIn.seen if m == "GET" and "readTime=" in p)
        self.assertRegex(read, r"/documents/read-time-\d+/d\?readTime=\d{4}-\d\d-\d\dT")
        self.assertEqual(StandIn.documents, {})

    def test_the_order_of_fields_is_compared_and_known(self):
        def case(name, written, read):
            database = f"projects/fo-{len(name)}/databases/(default)"
            time = "2026-10-11T08:29:42.523502Z"
            fields = lambda names: {n: {"stringValue": n} for n in names}
            return {"name": name, "steps": [
                {"method": "POST", "path": f"{database}/documents:commit", "status": 200,
                 "body": {"writes": [{"update": {"name": f"{database}/documents/c/d", "fields": fields(written)}}]},
                 "response": {"writeResults": [{"updateTime": time}], "commitTime": time}},
                {"method": "GET", "path": f"{database}/documents/c/d", "body": None, "status": 200,
                 "response": {"name": f"{database}/documents/c/d", "fields": fields(read), "createTime": time,
                              "updateTime": time}}]}
        recording = [case("in the written order", ["zeta", "alpha"], ["zeta", "alpha"]),
                     case("in another order", ["zeta", "alpha"], ["alpha", "zeta"]),
                     case("other fields", ["zeta", "alpha"], ["zeta", "beta"])]
        path = Path(self.dir.name) / "field_order.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["cases"], result["same"], result["known"]), (3, 1, {"D115": 1}))
        self.assertEqual([d["name"] for d in result["differ"]], ["other fields, step 2"])
        self.assertEqual(replay.order_difference({"f": {"a": 1, "b": 2}}, {"f": {"b": 2, "a": 1}}),
                         "$.f: keys in the order ['a', 'b'], production ['b', 'a']")

    def test_vector_search_is_replayed(self):
        vector = {"mapValue": {"fields": {"__type__": {"stringValue": "__vector__"},
                                          "value": {"arrayValue": {"values": [{"doubleValue": 1}]}}}}}
        one = {"integerValue": "1"}
        recording = {"seed": {"v/a": {"v": vector, "k": one}, "v/b": {"v": vector}}, "queries": [
            {"name": "nearest", "query": {"from": [{"collectionId": "v"}], "findNearest": {}},
             "outcome": [{"name": "v/a", "fields": {"k": one}}, {"name": "v/b", "fields": {}}]},
            {"name": "needs an index", "query": {"from": [{"collectionId": "ix"}], "findNearest": {}}, "outcome": []},
        ], "aggregations": [
            {"name": "count", "request": {"structuredAggregationQuery": {"structuredQuery": {"from": [{"collectionId": "v"}]},
                                                                        "aggregations": [{"alias": "n", "count": {}}]}},
             "outcome": [{"n": {"integerValue": "2"}}]},
        ], "writes": [
            {"name": "an array in an array", "fields": {"a": {"arrayValue": {"values": [{"arrayValue": {}}]}}},
             "outcome": {"status": 400, "message": "Nested arrays are not allowed"}},
        ]}
        path = Path(self.dir.name) / "find_nearest.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["cases"], result["sent"], result["same"], result["differ"]), (4, 4, 3, []))
        self.assertEqual(result["needsIndex"], ["needs an index"])
        self.assertEqual(len(result["indexes"]), 1)
        self.assertEqual(StandIn.documents, {})

    def test_emptying_reads_the_names_in_pages_and_puts_none_in_a_url(self):
        self.addCleanup(setattr, replay, "EMPTYING_PAGE", replay.EMPTYING_PAGE)
        replay.EMPTYING_PAGE = 2
        root = f"projects/{PROJECT}/databases/(default)/documents"
        StandIn.documents = {f"{root}/{n}": {"fields": {}} for n in ("a/1", "a/2", "a/1/s/x", "b/\u0000", "ab/1")}
        production = replay.Production(self.base, PROJECT, "a-token", 100)
        self.assertEqual(production.empty("a"), 3)
        self.assertEqual(sorted(StandIn.documents), [f"{root}/ab/1", f"{root}/b/\u0000"])
        self.assertEqual(production.empty(), 2)
        self.assertEqual(StandIn.documents, {})
        self.assertFalse([p for _, p, _, _ in StandIn.seen if "%00" in p])

    def test_what_the_official_emulator_ignores_is_sent_in_its_rest_form(self):
        documents = "projects/unsupported/databases/(default)/documents"
        recording = {"grpc": [
            {"name": "RunQuery", "rpc": "RunQuery", "authorization": "Bearer owner",
             "request": {"parent": documents, "structured_query": {"from": [{"collection_id": "c"}]}},
             "outcome": {"documents": ["c/a", "c/b"], "aggregations": [], "explain_metrics": False, "done": True}},
            {"name": "RunQuery, explain", "rpc": "RunQuery", "authorization": "Bearer owner",
             "request": {"parent": documents, "structured_query": {"from": [{"collection_id": "c"}]}, "explain_options": {}},
             "outcome": {"documents": ["c/a"], "aggregations": [], "explain_metrics": False, "done": True}},
            {"name": "anonymous", "rpc": "RunQuery", "authorization": None, "request": {"parent": documents},
             "outcome": {"code": "PERMISSION_DENIED", "message": "denied"}},
        ], "rest": [
            {"name": ":runQuery", "verb": "runQuery", "body": {"structuredQuery": {"from": [{"collectionId": "c"}]}},
             "outcome": {"status": 200, "body": json.dumps([{"document": {
                 "name": f"{documents}/c/{x}", "fields": {"n": {"integerValue": str(n)}}, "createTime": "<time>",
                 "updateTime": "<time>"}, "readTime": "<time>"} for n, x in ((1, "a"), (2, "b"))])}},
        ]}
        path = Path(self.dir.name) / "unsupported.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100), str(path))
        self.assertEqual((result["cases"], result["sent"], result["same"]), (4, 3, 2))
        self.assertEqual(result["skipped"], {"not sent as the owner": 1})
        self.assertEqual((result["known"], result["differ"]), ({"D079": 1}, []))
        self.assertEqual(StandIn.documents, {})

    def test_listen_targets_are_replayed_over_webchannel(self):
        def case(name, collection, outcome):
            return {"name": name, "parent": "", "query": {"from": [{"collectionId": collection}]}, "outcome": outcome}
        recording = {"dataset": [{"name": f"q/{x}", "fields": {"v": {"integerValue": "1"}}} for x in "ab"], "cases": [
            case("all of q", "q", {"documents": ["q/a", "q/b"]}),
            case("a projection", "sel", {"documents": []}),
            case("needs an index", "ix", {"documents": []}),
            case("differs", "q", {"documents": ["q/a"]}),
            {"name": "another project", "parent": "", "query": {"from": [{"collectionId": "q"}], "where": {"fieldFilter": {
                "field": {"fieldPath": "__name__"}, "op": "EQUAL",
                "value": {"referenceValue": "projects/other/databases/(default)/documents/q/a"}}}},
             "outcome": {"documents": []}},
        ]}
        path = Path(self.dir.name) / "listen_queries.json"
        path.write_text(json.dumps(recording))
        production = replay.Production(self.base, PROJECT, "a-token", 100)
        result = replay.replay(production, str(path))
        self.assertEqual((result["cases"], result["sent"], result["same"], result["known"]), (5, 4, 1, {"D117": 1}))
        self.assertEqual(result["needsIndex"], ["needs an index"])
        self.assertEqual([(d["name"], d["difference"]) for d in result["differ"]], [("differs", "$.documents: 1 items, production 2")])
        self.assertEqual(result["skipped"], {"names another project": 1})
        # The token goes in the first request's headers, as the SDK sends it; three requests a stream.
        self.assertEqual(StandIn.listened, ["Authorization:Bearer a-token\r\n"] * 4)
        self.assertEqual(production.firestore, len([p for _, p, _, _ in StandIn.seen]))
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
            # The Rules API has no `debug()` (D113).
            {"name": "debug", "condition": "debug(true)", "outcome": "allow"},
        ]}
        path = Path(self.dir.name) / "rules_eval.json"
        path.write_text(json.dumps(recording))
        production = replay.Production(self.base, PROJECT, "a-token", 100, rules=self.base)
        result = replay.replay(production, str(path))
        self.assertEqual((result["sent"], result["same"]), (6, 4))
        self.assertEqual((result["knownCases"], result["known"]), (1, {"D113": 1}))
        self.assertEqual([d["name"] for d in result["differ"]], ["differs"])
        self.assertEqual(result["differ"][0]["difference"], '$.outcome: "allow", production "deny"')
        self.assertEqual(result["skipped"], {"reads the document, the request or other documents": 1,
                                             "a batch of writes": 1})

    def test_rulesets_are_compiled_with_the_rules_api(self):
        def ruleset(condition):
            return "service cloud.firestore {\n  match /rooms/{room} {\n    allow read: if %s;\n  }\n}\n" % condition
        at = {"line": 5, "column": 22, "currentOffset": 137, "endOffset": 139}
        warned = {"issues": [{"sourcePosition": {"fileName": "firestore.rules", **at},
                              "description": "Invalid function name: foo.", "severity": "WARNING"}]}
        files = {"files": [{"name": "a.rules", "content": ruleset("true")}]}
        recording = [
            {"name": "compiles", "content": ruleset("true"), "status": 200, "response": {}},
            {"name": "a warning", "content": ruleset("foo()"), "status": 200, "response": warned},
            {"name": "does not compile", "content": ruleset("bad( && foo()"), "status": 400, "response": {"error": {
                "code": 400, "message": "Error compiling rules:\nL5:22 mismatched input\nL5:22 Invalid function name: foo.",
                "status": "INVALID_ARGUMENT"}}},
            {"name": "differs", "content": ruleset("false"), "status": 200, "response": warned},
            {"name": "empty", "content": "", "status": 400, "response": {"error": {
                "code": 400, "message": "Error compiling rules:\nL0:0 Rules content empty. Compilation aborted.",
                "status": "INVALID_ARGUMENT"}}},
            {"name": "one file", "request": {"rules": files}, "status": 200, "response": {}},
            {"name": "an unknown field", "request": {"rules": files, "x": 1}, "status": 400, "response": {"error": {
                "code": 400, "message": "Payload isn't valid for request.", "status": "INVALID_ARGUMENT"}}},
            {"name": "two files", "request": {"rules": {"files": files["files"] * 2}}, "status": 200, "response": {}},
            {"name": "no files", "request": {"rules": {"files": []}}, "status": 500, "response": {}},
            {"name": "not JSON", "raw": "not json", "status": 400, "response": {}},
        ]
        path = Path(self.dir.name) / "rules_compile.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100, rules=self.base), str(path))
        self.assertEqual((result["cases"], result["sent"], result["same"]), (10, 7, 4))
        # The error of the emulator's own request, and one of the Rules API (D097); a file without
        # content, which the Rules API refuses (D114).
        self.assertEqual((result["knownCases"], result["known"]), (2, {"D097": 1, "D114": 1}))
        self.assertEqual([(d["name"], d["difference"]) for d in result["differ"]], [
            ("differs", "$.issues: 1 items, production 0")])
        self.assertEqual(result["skipped"], {"several files: the Rules API tests one": 1, "the emulator's own request": 2})
        # Without a test suite, which compiles the ruleset; the request's `rules` are the source.
        sent = [json.loads(body) for _, _, _, body in StandIn.seen]
        self.assertTrue(all("testSuite" not in body for body in sent))
        self.assertIn({"source": files, "x": 1}, sent)

    def test_warnings_are_compared_with_their_positions(self):
        template = "service cloud.firestore {\n  match /rooms/{room} {\n    allow read: if %s;\n  }\n}\n"
        recording = {"rules": template, "results": [
            {"name": "none", "condition": "true"},
            {"name": "a warning", "condition": "foo()", "issues": [[5, 22, 137, 139, "WARNING", "Invalid function name: foo."]]},
            {"name": "an error", "condition": "bad(", "error": "Error compiling rules:\nL5:22 mismatched input"},
            {"name": "elsewhere", "condition": "foo()", "issues": [[5, 23, 138, 140, "WARNING", "Invalid function name: foo."]]},
            {"name": "no position", "condition": "false && 1", "issues": [[None, None, None, None, "WARNING", "Invalid type."]]},
            {"name": "a whole ruleset", "rules": template.replace("%s", "true")},
        ]}
        path = Path(self.dir.name) / "rules_warnings.json"
        path.write_text(json.dumps(recording))
        # Tests of rules are not in the budget, which is Firestore's.
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 0, rules=self.base), str(path))
        self.assertEqual((result["sent"], result["same"]), (6, 5))
        self.assertEqual([(d["name"], d["difference"]) for d in result["differ"]], [("elsewhere", "$.issues[0][1]: 23, production 22")])
        self.assertEqual(result["differ"][0]["rules"], "foo()")

    def test_paths_are_read_with_a_test_case_per_document(self):
        recording = {"documents": ["c/d", "c/x"], "results": [
            {"version": 2, "match": ["/c/{d}"], "condition": "true", "reads": {"c/d": "allow", "c/x": "allow"}},
            {"version": 1, "match": ["/c/{d}", "/{rest=**}"], "condition": "d == 'x'", "reads": {"c/d": "deny", "c/x": "allow"}},
            {"version": 2, "match": ["/c/{d}"], "condition": "foo()", "issues": ["Invalid function name: foo."],
             "reads": {"c/d": "deny", "c/x": "deny"}},
            {"version": 2, "match": ["/c/{d}"], "condition": "bad(",
             "compile": {"status": 400, "message": "Error compiling rules:\nL5:22 mismatched input"}},
            {"version": 2, "match": ["/c/{d}"], "condition": "false", "reads": {"c/d": "allow", "c/x": "deny"}},
            {"version": 2, "match": ["/c/{d}"], "condition": "debug(true)", "reads": {"c/d": "allow", "c/x": "allow"}},
        ]}
        path = Path(self.dir.name) / "rules_paths.json"
        path.write_text(json.dumps(recording))
        result = replay.replay(replay.Production(self.base, PROJECT, "a-token", 100, rules=self.base), str(path))
        self.assertEqual((result["sent"], result["same"]), (6, 4))
        self.assertEqual((result["knownCases"], result["known"]), (1, {"D113": 1}))
        self.assertEqual([(d["name"], d["difference"]) for d in result["differ"]],
                         [("version 2, /c/{d}: false", '$.reads.c/d: "allow", production "deny"')])
        # The ruleset `rules_paths.py` loaded, and a `get` of each document without `auth`.
        sent = next(b for b in map(json.loads, (body for _, _, _, body in StandIn.seen)) if "'x'" in json.dumps(b))
        self.assertEqual(sent["source"]["files"][0]["content"], (
            "service cloud.firestore {\n  match /databases/{database}/documents {\n    match /c/{d} {\n"
            "      match /{rest=**} {\n        allow read: if d == 'x';\n      }\n    }\n  }\n}\n"))
        self.assertEqual(sent["testSuite"]["testCases"], [
            {"expectation": "ALLOW", "request": {"auth": None, "method": "get", "path": f"/databases/(default)/documents/{d}"}}
            for d in ("c/d", "c/x")])

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


    def deploy(self, *fixtures, shard=None):
        """Runs the replay with rules deployed (`--deploy-rules`); its report."""
        out = Path(self.dir.name) / "out"
        os.environ[replay.TOKEN_ENV] = "a-token"
        try:
            replay.main(["--project", PROJECT, "--out", str(out), "--base", self.base, "--rules-base", self.base,
                         "--deploy-rules", *(["--shard", shard] if shard else []), *fixtures])
        finally:
            del os.environ[replay.TOKEN_ENV]
        return json.loads((out / "report.json").read_text())

    def rules_recording(self, cases):
        path = Path(self.dir.name) / "rules_request.json"
        path.write_text(json.dumps(cases))
        return str(path)

    def test_recordings_of_rules_are_sent_with_their_rules_deployed(self):
        def step(method, path, auth, status, response=None, body=None):
            return {"method": method, "path": path, "body": body, "auth": auth, "status": status, "response": response}

        def load(condition):
            rules = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
                     f"    match /c/{{d}} {{\n      allow read: if {condition};\n    }}\n  }}\n}}\n")
            return step("PUT", "/emulator/v1/projects/req:securityRules", None, 200, {},
                        {"rules": {"files": [{"name": "firestore.rules", "content": rules}]}})
        d, owner = "/v1/projects/req/databases/(default)/documents", "Bearer owner"
        write = step("PATCH", f"{d}/c/d", owner, 200, None, {"fields": {"n": {"integerValue": "1"}}})
        denied = {"error": {"code": 403, "message": "\nfalse for 'get' @ L5", "status": "PERMISSION_DENIED"}}
        cases = [
            {"name": "allowed", "steps": [write, load("true"), step("GET", f"{d}/c/d", None, 200)]},
            {"name": "missing", "steps": [load("true"), step("GET", f"{d}/c/x", None, 404)]},
            {"name": "denied", "steps": [write, load("false"), step("GET", f"{d}/c/d", None, 403, denied)]},
            {"name": "differs", "steps": [write, load("false"), step("GET", f"{d}/c/d", None, 200)]},
            {"name": "a user's", "steps": [load("true"), step("GET", f"{d}/c/d", "Bearer e30.e30.", 200)]},
            {"name": "coverage", "steps": [load("true"), step("GET", "/emulator/v1/projects/req:ruleCoverage", None, 200)]},
            {"name": "too early", "steps": [step("GET", f"{d}/c/d", None, 404), load("true")]},
        ]
        report = self.deploy(self.rules_recording(cases))
        result = report["recordings"][0]
        self.assertEqual((result["sent"], result["same"], result["knownCases"], result["known"]), (4, 2, 1, {"D118": 1}))
        self.assertEqual([d["name"] for d in result["differ"]], ["differs, step 3"])
        self.assertEqual(result["skipped"], {"sends a user's token": 1, "uses the official emulator's own API": 1,
                                             "sends a request before it loads rules": 1})
        # A ruleset for each, and again for the case that differed; each marked, and all gone at
        # the end with the release.
        self.assertEqual(len(StandIn.released), 3)
        self.assertTrue(all(content.endswith(release.MARK) for content in StandIn.released))
        self.assertEqual((StandIn.release, StandIn.rulesets), (None, {}))
        # The owner's steps carry the token, the others no credentials.
        sent = [(m, p.rsplit("/", 1)[-1], auth) for m, p, auth, _ in StandIn.seen if "/documents/c/" in p]
        self.assertIn(("PATCH", "d", "Bearer a-token"), sent)
        self.assertIn(("GET", "d", None), sent)
        self.assertNotIn(("GET", "d", "Bearer a-token"), sent)
        self.assertEqual(report["deployed"], "0/1")
        self.assertIn("rules deployed", (Path(self.dir.name) / "out" / "report.md").read_text())

    def test_a_case_sent_before_its_release_reached_every_server_is_sent_again(self):
        StandIn.lag = 1
        rules = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
                 "    match /c/{d} {\n      allow read: if true;\n    }\n  }\n}\n")
        case = {"name": "allowed", "steps": [
            {"method": "PUT", "path": "/emulator/v1/projects/req:securityRules", "auth": None, "status": 200, "response": {},
             "body": {"rules": {"files": [{"name": "firestore.rules", "content": rules}]}}},
            {"method": "GET", "path": "/v1/projects/req/databases/(default)/documents/c/x", "auth": None, "status": 404,
             "body": None, "response": None}]}
        report = self.deploy(self.rules_recording([case]))
        result = report["recordings"][0]
        self.assertEqual((result["same"], result["settled"], result["differ"]), (1, 1, []))
        self.assertIn("were the same when sent again", (Path(self.dir.name) / "out" / "report.md").read_text())

    def test_conditions_that_read_are_deployed(self):
        rules = ("rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
                 "    match /rooms/{room} {\n      allow read: if %s;\n    }\n  }\n}\n")
        recording = {
            "document": {"n": {"integerValue": "1"},
                         "ref": {"referenceValue": "projects/rules-eval/databases/(default)/documents/rooms/r2"}},
            "rules": rules,
            "results": [
                {"name": "a", "condition": "resource.data.n == 1", "outcome": "allow"},
                {"name": "b", "condition": "resource.data.n == 2", "outcome": "deny", "message": "\nfalse for 'get' @ L5"},
                {"name": "c", "condition": "request.path[0] == 'databases'", "outcome": "allow"},
                {"name": "d", "condition": "true", "outcome": "allow"},
                {"name": "e", "condition": "resource.data.n == 1 && bad(", "outcome": "compile", "message": "x"},
                {"name": "f", "condition": "resource.data.n == 1 && foo()", "outcome": "deny",
                 "issues": ["Invalid function name: foo."]},
                {"name": "batch", "condition": "getAfter(/databases/$(database)/documents/c/$(d)) != null",
                 "writes": ["+w"], "outcome": "allow"},
            ]}
        path = Path(self.dir.name) / "rules_eval.json"
        path.write_text(json.dumps(recording))
        result = self.deploy(str(path))["recordings"][0]
        self.assertEqual((result["sent"], result["same"]), (5, 4))
        self.assertEqual([d["name"] for d in result["differ"]], ["c"])
        self.assertEqual(result["skipped"], {"a batch of writes": 1, "tested with projects.test, not deployed": 1})
        # Those that do not depend on where the document is, together, each on a collection of its
        # own; the other alone, then again; the one that does not compile not at all.
        self.assertEqual(len(StandIn.released), 3)
        self.assertEqual(re.findall(r"match /(\w+)/", StandIn.released[0]), ["databases", "e0", "e1", "e4"])
        self.assertIn("match /rooms/{room}", StandIn.released[1])
        # The document is written in the replay's project, its reference too.
        commits = [b for m, p, _, b in StandIn.seen if p.endswith(":commit") and b and "rooms/r1" in b]
        self.assertIn(f"projects/{PROJECT}/databases/(default)/documents/rooms/r2", commits[0])
        self.assertNotIn("rules-eval", commits[0])

    def test_rules_the_replay_did_not_make_are_left_alone(self):
        theirs = f"projects/{PROJECT}/rulesets/theirs"
        StandIn.rulesets, StandIn.release = {theirs: "rules_version = '2';\n"}, theirs
        with self.assertRaisesRegex(RuntimeError, "did not make"):
            self.deploy(self.rules_recording([]))
        self.assertEqual((StandIn.release, list(StandIn.rulesets)), (theirs, [theirs]))
        self.assertFalse([p for _, p, _, _ in StandIn.seen if "/databases/" in p])

    def test_what_a_stopped_run_left_is_taken_down(self):
        def left():
            mine = [f"projects/{PROJECT}/rulesets/left-{i}" for i in range(2)]
            StandIn.rulesets = {mine[0]: "a\n" + release.MARK, mine[1]: "b\n" + release.MARK,
                                f"projects/{PROJECT}/rulesets/theirs": "c\n"}
            StandIn.release = mine[0]
        left()
        self.deploy(self.rules_recording([]))
        self.assertEqual((StandIn.release, list(StandIn.rulesets)), (None, [f"projects/{PROJECT}/rulesets/theirs"]))
        # As the workflow takes them down after a run, whatever happened, isolated (`-I`).
        left()
        script = Path(__file__).resolve().parent / "release.py"
        done = subprocess.run([sys.executable, "-I", "-B", str(script), "--project", PROJECT, "--rules-base", self.base],
                              env={**os.environ, replay.TOKEN_ENV: "a-token"}, capture_output=True, text=True)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual((StandIn.release, list(StandIn.rulesets)), (None, [f"projects/{PROJECT}/rulesets/theirs"]))

    def test_shards_share_out_what_deploys_rules(self):
        production = replay.Production(self.base, PROJECT, "a-token", 100)
        keys = [f"resource.data.n == {i}" for i in range(50)]
        shared = []
        for i in range(7):
            production.shard = (i, 7)
            shared += [k for k in keys if replay.in_shard(production, k)]
        self.assertEqual(sorted(shared), sorted(keys))
        self.assertTrue(0 < len([k for k in keys if replay.in_shard(production, k)]) < len(keys))
        for wrong in (["--shard", "7/7", "--deploy-rules"], ["--shard", "1/7"], ["--shard", "x"]):
            with self.assertRaises(SystemExit):
                replay.main(["--project", PROJECT, "--out", self.dir.name, *wrong, "f.json"])

if __name__ == "__main__":
    unittest.main()
