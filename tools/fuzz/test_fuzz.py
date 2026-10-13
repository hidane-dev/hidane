"""Offline tests of the differential fuzzer: python3 -m unittest discover -s tools/fuzz

The end-to-end test runs `fuzz.py run` against two small HTTP servers in this process, one
standing in for each emulator; nothing here needs Java, hidane or the network.
"""

import contextlib
import io
import json
import re
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import diff  # noqa: E402
import fuzz  # noqa: E402
import gen  # noqa: E402
import shrink  # noqa: E402

ERROR = '{\n  "error": {\n    "code": %d,\n    "message": "%s",\n    "status": "%s"\n  }\n}\n'


def answer(status, body, content_type="application/json; charset=UTF-8"):
    return {"status": status, "contentType": content_type, "body": body}


def error(status, message, code="INVALID_ARGUMENT"):
    return answer(status, ERROR % (status, message, code))


STEP = {"method": "POST", "path": "/v1/projects/{project}/databases/(default)/documents:commit"}


class Generation(unittest.TestCase):
    def test_a_case_is_the_same_for_the_same_seed_and_index(self):
        for index in range(len(gen.AREAS) * 3):
            self.assertEqual(gen.case(5, index), gen.case(5, index))
        self.assertNotEqual(gen.case(5, 0), gen.case(6, 0))

    def test_areas_take_turns_and_steps_are_requests(self):
        seen = set()
        for index in range(len(gen.AREAS) * 20):
            case = gen.case(11, index)
            seen.add(case["area"])
            self.assertEqual(case["area"], gen.AREAS[index % len(gen.AREAS)])
            json.dumps(case)  # plain JSON, so it can be written and replayed
            for step in case["steps"]:
                self.assertIn(step["method"], {"GET", "POST", "PATCH", "DELETE", "PUT"})
                self.assertTrue(step["path"].startswith(("/v1/", "/emulator/v1/")), step["path"])
                self.assertTrue(step["path"].isascii(), step["path"])
                self.assertFalse("body" in step and "raw" in step)
        self.assertEqual(seen, set(gen.AREAS))
        self.assertEqual({gen.case(1, i, ("rules",))["area"] for i in range(5)}, {"rules"})

    def test_no_request_the_official_emulator_never_answers(self):
        # docs/parity-exceptions.md: GET with ?transaction=, query parameters that do not parse,
        # a parent in a listCollectionIds body, GET //.
        for index in range(len(gen.AREAS) * 40):
            for step in gen.case(3, index)["steps"]:
                path = step["path"]
                if step["method"] == "GET":
                    self.assertNotIn("transaction=", path)
                self.assertIsNone(re.search(r"pageSize=[^0-9]|showMissing=(?!true|false)", path), path)
                if path.endswith(":listCollectionIds"):
                    self.assertNotIn("parent", step.get("body", {}))
                self.assertFalse(path.startswith("//"))

    def test_generated_times_avoid_the_years_masked_as_the_servers(self):
        g = gen.Gen(__import__("random").Random(1))
        for _ in range(2000):
            self.assertIsNone(re.match(r"20(2[1-9]|[3-9][0-9])-", g.timestamp()))

    def test_field_paths_quote_what_is_not_an_identifier(self):
        self.assertEqual(gen.field_path("a", "b_1"), "a.b_1")
        self.assertEqual(gen.field_path("x y", "é", "a`b", "1"), "`x y`.`é`.`a\\`b`.`1`")


class Normalisation(unittest.TestCase):
    def test_masks_what_a_run_chose(self):
        values = {"project": "fabc-123456", "tx": "EWQAAAAAAAAB", "commitTime": "1970-01-01T00:00:00Z"}
        text = ('{"name": "projects/fabc-123456/databases/(default)/documents/c/d", "updateTime": "x", '
                '"t": {"timestampValue": "2026-10-13T02:57:10.579Z"}, "old": {"timestampValue": "2020-01-01T00:00:00Z"}, '
                '"d": {"doubleValue": 2.2250738585072014E-308}, '
                '"message": "tx EWQAAAAAAAAB: the stored version (1791600063752288) does not match (978307200000000)"}')
        out = diff.normalize(text, values)
        self.assertIn("projects/{project}/", out)
        self.assertIn('"updateTime": "<time>"', out)
        self.assertIn('"timestampValue": "<now>"', out)
        self.assertIn("2020-01-01T00:00:00Z", out)
        self.assertIn("tx <tx>", out)
        self.assertIn("version (<version>) does not match (978307200000000)", out)
        self.assertIn("2.2250738585072014E-308", out)

    def test_fills_placeholders_per_target(self):
        values = {"project": "p", "tx": "a+b/="}
        self.assertEqual(diff.fill("/v1/projects/{project}/x?t={tx}", values, True), "/v1/projects/p/x?t=a%2Bb%2F%3D")
        self.assertEqual(diff.fill_json({"transaction": "{tx}", "n": [1, "{project}"]}, values),
                         {"transaction": "a+b/=", "n": [1, "p"]})

    def test_captures_transactions_tokens_and_times(self):
        values = {}
        diff.capture('[{"transaction": "T1", "readTime": "x"}]', values)
        diff.capture('{"writeResults": [{"updateTime": "U"}], "commitTime": "C"}', values)
        diff.capture('{"documents": [], "nextPageToken": "N"}', values)
        diff.capture("not json", values)
        self.assertEqual(values, {"tx": "T1", "updateTime": "U", "commitTime": "C", "token": "N"})

    def test_project_ids_are_fresh_and_as_long_as_the_recordings(self):
        ids = {diff.project() for _ in range(50)}
        self.assertEqual(len(ids), 50)
        for p in ids:
            self.assertEqual(len(p), len("rest-000000"))
            self.assertRegex(p, r"^[a-z][a-z0-9-]+$")


class Classification(unittest.TestCase):
    def test_the_same_answers_do_not_differ(self):
        self.assertIsNone(diff.classify(STEP, answer(200, "{}"), answer(200, "{}")))

    def test_crash(self):
        found = diff.classify(STEP, answer(200, "{}"), error(500, "boom", "INTERNAL"))
        self.assertEqual((found["kind"], found["detail"]), ("crash", "hidane:200/500"))
        found = diff.classify(STEP, {"status": None, "error": "TimeoutError"}, error(400, "x"))
        self.assertEqual((found["kind"], found["official"]), ("crash", {"status": None, "error": "TimeoutError"}))

    def test_status(self):
        self.assertEqual(diff.classify(STEP, error(400, "x"), answer(200, "{}"))["kind"], "status")
        found = diff.classify(STEP, error(400, "x"), error(400, "x", "FAILED_PRECONDITION"))
        self.assertEqual((found["kind"], found["detail"]), ("status", "INVALID_ARGUMENT/FAILED_PRECONDITION"))

    def test_message(self):
        found = diff.classify(STEP, error(400, "Bad 'a' at 3"), error(400, "Wrong"))
        self.assertEqual((found["kind"], found["official"]["message"], found["hidane"]["code"]),
                         ("message", "Bad 'a' at 3", "INVALID_ARGUMENT"))
        # An error inside a stream's array is still an error.
        streamed = answer(400, "[" + ERROR % (400, "a", "INVALID_ARGUMENT") + "]")
        self.assertEqual(diff.classify(STEP, streamed, error(400, "b"))["kind"], "message")

    def test_order_of_list_elements_and_of_object_keys(self):
        found = diff.classify(STEP, answer(200, '[{"a": 1}, {"b": 2}]'), answer(200, '[{"b": 2}, {"a": 1}]'))
        self.assertEqual((found["kind"], found["detail"]), ("order", ""))
        found = diff.classify(STEP, answer(200, '{"d": {"x": 1, "y": 2}}'), answer(200, '{"d": {"y": 2, "x": 1}}'))
        self.assertEqual((found["kind"], found["detail"]), ("order", "d"))

    def test_data_names_where_and_what(self):
        o = answer(200, '{"document": {"fields": {"zz": {"doubleValue": 1.0}}}}')
        h = answer(200, '{"document": {"fields": {"zz": {"doubleValue": 1}}}}')
        found = diff.classify(STEP, o, h)
        self.assertEqual((found["kind"], found["detail"]), ("data", "document.fields.*.doubleValue"))
        self.assertEqual((found["official"]["value"], found["hidane"]["value"]), ("1.0", "1"))
        found = diff.classify(STEP, answer(200, '[{"a": 1}]'), answer(200, '[{"a": 1}, {"a": 1}]'))
        self.assertEqual((found["kind"], found["detail"]), ("data", ""))
        found = diff.classify(STEP, answer(200, '{"a": 1}'), answer(200, '{"a": 1, "done": true}'))
        self.assertEqual(found["detail"], "done")
        self.assertEqual(diff.classify(STEP, answer(200, '{"a": "<"}'), answer(200, '{"a": "\\u003c"}'))["detail"], "layout")
        found = diff.classify(STEP, answer(404, "Not Found", "text/plain"), answer(404, "{}"))
        self.assertEqual((found["kind"], found["detail"]), ("data", "content type"))

    def test_signatures_ignore_the_inputs_values_and_the_area(self):
        one = diff.compare({"area": "batches", "steps": [STEP]}, [error(400, "Bad 'a' at 3")], [error(400, "Wrong")])
        two = diff.compare({"area": "malformed", "steps": [STEP]}, [error(400, "Bad 'zz' at 15")], [error(400, "Wrong")])
        self.assertEqual(one["signature"], two["signature"])
        self.assertRegex(one["signature"], fuzz.SIGNATURE)
        self.assertTrue(one["signature"].startswith("message-commit-"))
        other = diff.compare({"area": "batches", "steps": [STEP]}, [error(400, "Bad")], [error(400, "Wrong")])
        self.assertNotEqual(one["signature"], other["signature"])

    def test_compare_finds_the_first_step_that_differs(self):
        case = {"area": "documents", "steps": [STEP, STEP, STEP]}
        found = diff.compare(case, [answer(200, "{}"), error(400, "a"), error(400, "x")],
                             [answer(200, "{}"), error(400, "b"), answer(200, "{}")])
        self.assertEqual((found["step"], found["kind"]), (1, "message"))
        found = diff.compare(case, [answer(200, "{}")], [answer(200, "{}"), answer(200, "{}")])
        self.assertEqual((found["step"], found["kind"], found["official"]["error"]), (1, "crash", "missing"))

    def test_verbs(self):
        base = "/v1/projects/{project}/databases/(default)/documents"
        self.assertEqual(diff.verb({"method": "POST", "path": base + ":runQuery"}), ":runQuery")
        self.assertEqual(diff.verb({"method": "POST", "path": base + "/c/d:runQuery"}), ":runQuery")
        self.assertEqual(diff.verb({"method": "GET", "path": base + "/c/d?mask.fieldPaths=a"}), "GET document")
        self.assertEqual(diff.verb({"method": "POST", "path": base + "/c?documentId=x"}), "POST collection")
        self.assertEqual(diff.verb({"method": "PUT", "path": "/emulator/v1/projects/{project}:securityRules"}),
                         "PUT emulator:securityRules")

    def test_templates(self):
        self.assertEqual(diff.template('Document "projects/p/databases/(default)/documents/c" has 3 [a, b]'),
                         'Document "…" has N […]')
        denial = "\nevaluation error at L5:22 for 'get' @ L5, Property owner is undefined on object. for 'get' @ L5, false for 'list' @ L6"
        self.assertEqual(diff.template(denial), "\\nevaluation error; Property … is undefined on object.; false")
        self.assertEqual(diff.template("No matching allow statements"), "No matching allow statements")


DENIAL = {"status": 403, "code": "PERMISSION_DENIED"}


class Exceptions(unittest.TestCase):
    RULES = [{"id": "reasons", "exception": "D008", "why": "two passes", "kinds": ["message"],
              "official": {"status": 403}, "hidane": {"status": 403}, "relation": "hidane-reasons-subset"},
             {"id": "wording", "exception": "D064", "why": "wording", "kinds": ["message"],
              "official": {"message": "^entity already exists"}, "hidane": {"message": "^Document already exists"}},
             {"id": "batch-write", "exception": "D064", "why": "wording", "kinds": ["data"], "detail": r"^status\.\[\]\.message$",
              "official": {"value": "^entity"}, "hidane": {"value": "^Document"}}]

    def difference(self, kind, official, hidane, detail=""):
        return {"kind": kind, "verb": ":commit", "detail": detail, "official": official, "hidane": hidane}

    def test_rules_match_what_they_say(self):
        rules = diff.Exceptions(self.RULES)
        two_passes = self.difference("message", {**DENIAL, "message": "\nevaluation error at L5:22 for 'get' @ L5, false for 'get' @ L5"},
                                     {**DENIAL, "message": "\nfalse for 'get' @ L5"})
        self.assertEqual(rules.match("rules", two_passes)["id"], "reasons")
        other = self.difference("message", {**DENIAL, "message": "\nfalse for 'get' @ L5"},
                                {**DENIAL, "message": "\nfalse for 'list' @ L6"})
        self.assertIsNone(rules.match("rules", other))
        wording = self.difference("message", {"status": 409, "message": "entity already exists: EntityRef[x]"},
                                  {"status": 409, "message": "Document already exists: projects/p"})
        self.assertEqual(rules.match("documents", wording)["id"], "wording")
        statuses = self.difference("data", {"status": 200, "value": "entity already exists"},
                                   {"status": 200, "value": "Document already exists"}, r"status.[].message")
        self.assertEqual(rules.match("batches", statuses)["id"], "batch-write")
        self.assertIsNone(rules.match("batches", {**statuses, "detail": "writeResults"}))

    def test_check_finds_bad_rules(self):
        bad = diff.Exceptions([{"id": "Bad Id", "exception": "D999", "kinds": ["boom"], "verbs": "(",
                                "official": {"nope": 1, "message": "["}, "relation": "unknown", "extra": 1},
                               {"id": "broad", "exception": "D008", "why": "x"}])
        problems = "\n".join(bad.check({"D008"}))
        for expected in ("lowercase", "D999", "why", "boom", "verbs", "nope", "official.message", "unknown relation",
                         "unknown keys ['extra']", "too broad"):
            self.assertIn(expected, problems)
        self.assertEqual(diff.Exceptions(self.RULES).check({"D008", "D064"}), [])

    def test_the_rules_in_use_are_valid(self):
        rules = diff.Exceptions.load()
        ids = {rule["exception"] for rule in rules.rules}
        self.assertEqual(rules.check(ids), [])
        path = diff.ROOT / "docs" / "parity-exceptions.json"
        if path.exists():
            self.assertEqual(rules.check(diff.parity_entries(path)), [])


class Shrinking(unittest.TestCase):
    def test_ddmin_finds_the_needed_elements(self):
        calls = []

        def test(items):
            calls.append(items)
            return 3 in items and 7 in items
        self.assertEqual(shrink.ddmin(list(range(10)), test), [3, 7])
        self.assertEqual(shrink.ddmin([1, 2], lambda items: True, keep_empty=True), [])

    def test_reduce_json_keeps_what_the_test_needs(self):
        value = {"writes": [{"update": {"fields": {"a": 1, "bad": {"x": "y" * 100}, "c": [1, 2, 3]}}}, {"delete": "d"}]}

        def needs_bad(v):
            return "bad" in json.dumps(v) and isinstance(v, dict) and "writes" in v
        self.assertEqual(shrink.reduce_json(value, needs_bad),
                         {"writes": [{"update": {"fields": {"bad": {}}}}]})
        self.assertEqual(shrink.reduce_json("z" * 1000, lambda v: len(v) > 40), "z" * 62)

    def test_minimize_drops_steps_and_body_parts_and_respects_the_budget(self):
        case = {"name": "c", "area": "batches", "steps": [
            {"method": "POST", "path": "/a", "body": {"x": 1}},
            {"method": "POST", "path": "/b", "body": {"keep": 1, "drop": [1, 2, 3]}},
            {"method": "POST", "path": "/c", "body": {"bad": True, "noise": {"n": 1}}},
            {"method": "GET", "path": "/after"},
        ]}

        def reproduces(c):
            paths = [s["path"] for s in c["steps"]]
            return paths[-1] == "/c" and "/b" in paths and "keep" in c["steps"][paths.index("/b")]["body"] \
                and c["steps"][-1]["body"].get("bad")
        small, tests = shrink.minimize(case, 2, reproduces)
        self.assertEqual(small["steps"], [{"method": "POST", "path": "/b", "body": {"keep": 1}},
                                          {"method": "POST", "path": "/c", "body": {"bad": True}}])
        self.assertGreater(tests, 0)
        _, tests = shrink.minimize(case, 2, reproduces, budget=3)
        self.assertLessEqual(tests, 3)


class FakeEmulator(BaseHTTPRequestHandler):
    """Answers like a tiny emulator; the `hidane` one refuses less than the `official` one."""
    lenient = False

    def do_GET(self):
        self.reply(200, "Ok" if self.path == "/" else '{\n  "name": "x"\n}\n')

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0)).decode()
        if "NaN" in body and not self.lenient:
            self.reply(400, ERROR % (400, "NaN is not allowed here", "INVALID_ARGUMENT"))
        else:
            self.reply(200, '{\n  "commitTime": "2026-01-01T00:00:00.000001Z"\n}\n')

    do_PATCH = do_PUT = do_DELETE = do_POST

    def reply(self, status, text):
        data = text.encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=UTF-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


class Lenient(FakeEmulator):
    lenient = True


@contextlib.contextmanager
def serving(handler):
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"127.0.0.1:{server.server_address[1]}"
    finally:
        server.shutdown()
        server.server_close()


class EndToEnd(unittest.TestCase):
    def test_a_run_writes_minimised_differences_and_a_summary(self):
        with serving(FakeEmulator) as official, serving(Lenient) as hidane, tempfile.TemporaryDirectory() as out:
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = fuzz.main(["run", "--official", official, "--hidane", hidane, "--seed", "4", "--cases", "60",
                                  "--areas", "batches,transforms,documents", "--minimize-budget", "60", "--out", out])
            self.assertEqual(code, 0)
            summary = json.loads(Path(out, "summary.json").read_text())
            self.assertEqual(summary["cases"], 60)
            self.assertGreater(summary["differing_cases"], 0)
            self.assertEqual(set(summary["by_area"]), {"batches", "transforms", "documents"})
            unexplained = [d for d in summary["differences"] if d["outcome"] == "unexplained"]
            self.assertTrue(unexplained)
            for entry in unexplained:
                self.assertEqual(entry["kind"], "status")
                data = json.loads(Path(out, entry["file"]).read_text())
                self.assertEqual(data["generated_by"], "tools/fuzz/fuzz.py")
                self.assertIn("v1.22.0" if fuzz.pinned()[0] == "1.22.0" else "v", data["oracle"])
                # Minimised to the one step that carries a NaN, in the recordings' format.
                self.assertEqual(len(data["case"]["steps"]), 1)
                self.assertIn("NaN", json.dumps(data["case"]["steps"][0]))
                self.assertEqual(data["cases"][0]["outcome"]["status"], 400)
                self.assertEqual(data["hidane_outcomes"][0]["status"], 200)
                self.assertLessEqual(len(json.dumps(data["case"])), len(json.dumps(data["original"])))
            run = json.loads(Path(out, "run.json").read_text())
            for key in ("name", "date", "target", "official_emulator", "cases", "differences", "unexplained", "evidence"):
                self.assertIn(key, run)
            self.assertEqual(run["unexplained"], len(unexplained))
            self.assertIn("Differential fuzzing, seed 4", Path(out, "summary.md").read_text())
            self.assertIn("| batches |", stdout.getvalue())

    def test_replay_runs_a_difference_file_again(self):
        case = {"name": "x", "area": "batches", "steps": [
            {"method": "POST", "path": "/v1/projects/{project}/databases/(default)/documents:commit",
             "headers": {}, "body": {"writes": [{"update": {"fields": {"d": {"doubleValue": "NaN"}}}}]}}]}
        with serving(FakeEmulator) as official, serving(Lenient) as hidane, tempfile.TemporaryDirectory() as tmp:
            Path(tmp, "d.json").write_text(json.dumps({"case": case}))
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                fuzz.main(["replay", "--official", official, "--hidane", hidane, str(Path(tmp, "d.json"))])
            self.assertEqual(json.loads(stdout.getvalue())["difference"]["kind"], "status")

    def test_out_must_be_outside_the_repository_and_both_must_answer(self):
        with serving(FakeEmulator) as official, serving(Lenient) as hidane:
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                code = fuzz.main(["run", "--official", official, "--hidane", hidane, "--seed", "1", "--cases", "1",
                                  "--out", str(diff.ROOT / "fuzz-out")])
            self.assertEqual(code, 1)
            self.assertIn("outside the repository", stderr.getvalue())
            self.assertFalse((diff.ROOT / "fuzz-out").exists())
            with contextlib.redirect_stderr(io.StringIO()), tempfile.TemporaryDirectory() as out:
                self.assertEqual(fuzz.main(["run", "--official", "127.0.0.1:9", "--hidane", hidane, "--seed", "1",
                                            "--cases", "1", "--out", out]), 1)

    def test_record_adds_a_run_to_the_results(self):
        with tempfile.TemporaryDirectory() as tmp:
            results, original = Path(tmp, "differential-fuzzing.json"), fuzz.RESULTS
            fuzz.RESULTS = results
            try:
                entry = {"name": "REST, seed 1", "date": "2026-10-13", "target": "t", "cases": 1, "differences": 0,
                         "unexplained": 0, "evidence": "tools/fuzz/"}
                Path(tmp, "run.json").write_text(json.dumps(entry))
                with contextlib.redirect_stdout(io.StringIO()):
                    fuzz.main(["record", str(Path(tmp, "run.json"))])
                    fuzz.main(["record", str(Path(tmp, "run.json"))])
                self.assertEqual(json.loads(results.read_text())["runs"], [entry])
                Path(tmp, "bad.json").write_text("{}")
                with contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(fuzz.main(["record", str(Path(tmp, "bad.json"))]), 1)
            finally:
                fuzz.RESULTS = original

    def test_files_hold_lone_surrogates_as_escapes(self):
        with tempfile.TemporaryDirectory() as tmp:
            fuzz.write_json(Path(tmp, "a", "x.json"), {"s": "é\ud800"})
            self.assertEqual(json.loads(Path(tmp, "a", "x.json").read_text())["s"], "é\ud800")
            fuzz.write_json(Path(tmp, "y.json"), {"s": "é"})
            self.assertIn("é", Path(tmp, "y.json").read_text(encoding="utf-8"))

    def test_markdown_cells_cannot_break_the_table(self):
        self.assertEqual(fuzz.cell("a|b`c\nd<e>"), "a b c d e ")
        self.assertTrue(fuzz.cell("x" * 500).endswith("…"))


if __name__ == "__main__":
    unittest.main()
