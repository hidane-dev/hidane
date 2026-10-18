"""Offline tests of the differential fuzzer: python3 -m unittest discover -s tools/fuzz

The end-to-end test runs `fuzz.py run` against two small HTTP servers in this process, one
standing in for each emulator; nothing here needs Java, hidane or the network.
"""

import argparse
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
from unittest import mock

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
COMMIT = "0123456789abcdef0123456789abcdef01234567"


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

    def test_datastore_areas_take_turns_and_send_datastore_requests(self):
        areas = gen.MODES["datastore-mode"]
        self.assertFalse(set(areas) & set(gen.AREAS))
        seen = set()
        for index in range(len(areas) * 40):
            case = gen.case(7, index, areas)
            self.assertEqual(case, gen.case(7, index, areas))
            seen.add(case["area"])
            json.dumps(case)
            for step in case["steps"]:
                self.assertEqual(step["method"], "POST")
                self.assertRegex(step["path"], r"^/v1/projects/\{project\}:(lookup|runQuery|runAggregationQuery|"
                                               r"beginTransaction|commit|rollback|allocateIds|reserveIds)$")
                self.assertFalse("body" in step and "raw" in step)
                # No timestamp a value or a read time holds is in the years masked as the servers'.
                self.assertIsNone(re.search(r'"20(2[1-9]|[3-9][0-9])-', json.dumps(step.get("body"))))
        self.assertEqual(seen, set(areas))

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

    def test_captures_and_masks_datastore_cursors_and_versions(self):
        values = {"project": "fabc-123456"}
        diff.capture('{"batch": {"entityResults": [], "endCursor": "CgQSAgg="}, "transaction": "T2"}', values)
        diff.capture('{"mutationResults": [{"version": "1791600063752288"}], "commitTime": "C"}', values)
        self.assertEqual(values, {"project": "fabc-123456", "cursor": "CgQSAgg=", "tx": "T2", "version": "1791600063752288",
                                  "commitTime": "C"})
        out = diff.normalize('{"found": [{"version": "1791600063752288", "cursor": "CgQSAgg="}], '
                             '"batch": {"skippedCursor": "x", "snapshotVersion": "17"}, "a": "CgQSAgg="}', values)
        self.assertEqual(out, '{"found": [{"version": "<version>", "cursor": "<cursor>"}], '
                              '"batch": {"skippedCursor": "<cursor>", "snapshotVersion": "<version>"}, "a": "<cursor>"}')

    def test_project_ids_are_fresh_and_as_long_as_the_recordings(self):
        ids = {diff.project() for _ in range(50)}
        self.assertEqual(len(ids), 50)
        for p in ids:
            self.assertEqual(len(p), len("rest-000000"))
            self.assertRegex(p, r"^[a-z][a-z0-9-]+$")


class Spacing(unittest.TestCase):
    def test_a_request_time_step_waits_after_the_last_answer(self):
        """#329: REQUEST_TIME has millisecond precision; hidane answers fast enough for two
        commits to share one."""
        transform = {"method": "POST", "path": BASE + ":commit", "body": {"writes": [{"transform": {
            "document": "c/d", "fieldTransforms": [{"fieldPath": "t", "setToServerValue": "REQUEST_TIME"}]}}]}}
        sender = lambda *args: (200, "application/json", "{}")
        with mock.patch("time.sleep") as sleep:
            diff.run("host", {"steps": [patch({}), transform]}, sender=sender)
        self.assertEqual(len(sleep.call_args_list), 1)
        self.assertLessEqual(sleep.call_args[0][0], diff.REQUEST_TIME_GAP)
        with mock.patch("time.sleep") as sleep:
            diff.run("host", {"steps": [transform, patch({})]}, sender=sender)
        sleep.assert_not_called()


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
        self.assertEqual((found["kind"], found["detail"]), ("status", "INVALID_ARGUMENT/FAILED_PRECONDITION: x | x"))
        # The refusals' templates tell refusals of different things apart, not their values.
        unset = diff.compare({"steps": [STEP]}, [error(400, "Cannot convert a 'v' value.")], [answer(200, "{}")])
        nested = diff.compare({"steps": [STEP]}, [error(400, "Cannot transform property m")], [answer(200, "{}")])
        other = diff.compare({"steps": [STEP]}, [error(400, "Cannot convert a 'w' value.")], [answer(200, "{}")])
        self.assertEqual(unset["detail"], "INVALID_ARGUMENT/None: Cannot convert a '…' value.")
        self.assertNotEqual(unset["signature"], nested["signature"])
        self.assertEqual(unset["signature"], other["signature"])

    def test_generated_ids_in_another_order_do_not_differ(self):
        auto = '{"name": "projects/{project}/databases/(default)/documents/gen/<auto>"}'
        named = '{"name": "projects/{project}/databases/(default)/documents/gen/x"}'
        first, last = (answer(200, '{"documents": [%s, %s]}' % pair) for pair in ((auto, named), (named, auto)))
        self.assertIsNone(diff.classify(STEP, first, last))
        other = answer(200, '{"documents": [%s, %s]}' % (named, named.replace("/x", "/y")))
        self.assertEqual(diff.classify(STEP, other, answer(200, '{"documents": [%s, %s]}' % (
            named.replace("/x", "/y"), named)))["kind"], "order")

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
        # The Datastore API's verbs are not Firestore's.
        self.assertEqual(diff.verb({"method": "POST", "path": "/v1/projects/{project}:commit"}), "datastore:commit")
        self.assertEqual(diff.generalize(("found", 0, "entity", "properties", "a", "integerValue")),
                         "found.[].entity.properties.*.integerValue")

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
        # An evaluation error of the first pass stands for any reason of its statement, not another's.
        failed_first = "\nevaluation error at L8:31 for 'create' @ L8, evaluation error at L8:31 for 'update' @ L8"
        stands_for = self.difference("message", {**DENIAL, "message": failed_first + ", Null value error. for 'create' @ L8"},
                                     {**DENIAL, "message": "\nNull value error. for 'create' @ L8, false for 'update' @ L8"})
        self.assertEqual(rules.match("rules", stands_for)["id"], "reasons")
        elsewhere = self.difference("message", {**DENIAL, "message": failed_first},
                                    {**DENIAL, "message": "\nfalse for 'delete' @ L9"})
        self.assertIsNone(rules.match("rules", elsewhere))
        other = self.difference("message", {**DENIAL, "message": "\nfalse for 'get' @ L5"},
                                {**DENIAL, "message": "\nfalse for 'list' @ L6"})
        self.assertIsNone(rules.match("rules", other))
        # Another error of the first pass, whose second pass did not run (a batch decided on a
        # later document), stands for hidane's `false` for its statement, and only for that.
        unpassed = {**DENIAL, "message": "\nNull value error. for 'get' @ L5, false for 'get' @ L11"}
        absorbed = self.difference("message", unpassed, {**DENIAL, "message": "\nfalse for 'get' @ L5"})
        self.assertEqual(rules.match("rules", absorbed)["id"], "reasons")
        another_error = self.difference("message", unpassed, {**DENIAL, "message": "\nProperty n is undefined on object. for 'get' @ L5"})
        self.assertIsNone(rules.match("rules", another_error))
        another_statement = self.difference("message", unpassed, {**DENIAL, "message": "\nfalse for 'get' @ L6"})
        self.assertIsNone(rules.match("rules", another_statement))
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


BASE = "/v1/projects/{project}/databases/(default)/documents"
PAYLOAD = "Payload isn't valid for request."


def patch(fields, path=BASE + "/c/d", **step):
    return {"method": "PATCH", "path": path, "headers": {}, "body": {"fields": fields}, **step}


class RequestRules(unittest.TestCase):
    """Rules that look at the request as well as at the answers (`request`, `earlier`)."""

    def test_predicates(self):
        surrogate = patch({"v": {"stringValue": "a\ud800"}})
        self.assertTrue(diff.lone_surrogate(surrogate))
        self.assertTrue(diff.lone_surrogate(patch({"k\udc00": {"nullValue": None}})))
        self.assertTrue(diff.lone_surrogate({"method": "POST", "path": BASE + ":commit", "raw": '{"s": "\\ud800"}'}))
        self.assertFalse(diff.lone_surrogate(patch({"v": {"stringValue": "\U0001F600"}})))
        self.assertFalse(diff.lone_surrogate({"method": "POST", "path": BASE + ":commit", "raw": '{"s": "\\ud83d\\ude00"}'}))
        self.assertFalse(diff.lone_surrogate({"method": "GET", "path": BASE + "/c/d"}))

        def time(text):
            return patch({"t": {"timestampValue": text}})
        for lenient in ("2020-13-01T00:00:00Z", "2020-00-01T00:00:00Z", "2019-02-29T00:00:00Z", "2020-01-01T24:00:00Z",
                        "2020-01-01T00:60:00Z", "2020-01-01T00:00:61Z", "2020-01-01T00:00:00+24:00",
                        "2020-1-01T00:00:00Z", "20-01-01T00:00:00Z", "2020-01-01T00:00:00.Z", "2020-01-01T00:00:00+9:00"):
            self.assertTrue(diff.lenient_timestamp(time(lenient)), lenient)
        for strict in ("2020-12-31T23:59:59.999999999Z", "2020-02-29T00:00:00Z", "2016-12-31T23:59:60Z",
                       "2020-01-01T00:00:00+23:59", "2020-01-01T00:00:00.1234567891Z", "not a time",
                       "2020-01-01T00:00:00", "2020-01-01 00:00:00Z", "13"):
            self.assertFalse(diff.lenient_timestamp(time(strict)), strict)
        # In a precondition, a read time, a query parameter; not as a key.
        self.assertTrue(diff.lenient_timestamp({"method": "POST", "path": BASE + ":runQuery",
                                                "body": {"readTime": "2020-13-01T00:00:00Z"}}))
        self.assertTrue(diff.lenient_timestamp({"method": "PATCH", "path": BASE + "/c/d?currentDocument.updateTime="
                                                "2020-13-01T00%3A00%3A00Z", "body": {}}))
        self.assertFalse(diff.lenient_timestamp(patch({"2020-13-01T00:00:00Z": {"nullValue": None}})))

        # Each `{project}` counts as the fuzzer's 11 characters: 4096 bytes are served, 4097 are not.
        prefix = "GET " + BASE.replace("{project}", "p" * 11) + "/c/"
        fits = 4096 - len(prefix) - len(" HTTP/1.1")
        self.assertFalse(diff.long_request_line({"method": "GET", "path": BASE + "/c/" + "x" * fits}))
        self.assertTrue(diff.long_request_line({"method": "GET", "path": BASE + "/c/" + "x" * (fits + 1)}))

        commit = {"method": "POST", "path": BASE + ":commit", "body": {"writes": [{}, {}]}}
        self.assertTrue(diff.several_targets(commit))
        self.assertTrue(diff.several_targets({"method": "POST", "path": BASE + ":batchGet", "body": {"documents": ["a", "b"]}}))
        self.assertFalse(diff.several_targets({**commit, "body": {"writes": [{}]}}))
        self.assertFalse(diff.several_targets({"method": "GET", "path": BASE + "/c/d"}))

        query = {"method": "POST", "path": BASE + ":runQuery", "body": {"newTransaction": {"readWrite": {}}}}
        self.assertTrue(diff.in_transaction(query))
        self.assertTrue(diff.in_transaction({**query, "body": {"transaction": "x"}}))
        self.assertFalse(diff.in_transaction({**query, "body": {"newTransaction": {"readOnly": {}}}}))
        self.assertFalse(diff.in_transaction({**query, "body": {}}))
        self.assertFalse(diff.in_transaction({**query, "raw": "null", "body": None}))

        aggregation = lambda f: {"method": "POST", "path": "/v1/projects/{project}:runAggregationQuery", "body": {  # noqa: E731
            "aggregationQuery": {"nestedQuery": {"kind": [{"name": "K"}], "filter": f}, "aggregations": [{"count": {}}]}}}
        on = lambda op, v: {"propertyFilter": {"property": {"name": "a"}, "op": op, "value": v}}  # noqa: E731
        self.assertTrue(diff.entity_operand(aggregation(on("LESS_THAN", {"entityValue": {}}))))
        self.assertTrue(diff.entity_operand(aggregation({"compositeFilter": {"op": "AND", "filters": [
            on("EQUAL", {"integerValue": "1"}), on("IN", {"arrayValue": {"values": [{"entityValue": {}}]}})]}})))
        self.assertFalse(diff.entity_operand(aggregation(on("NOT_EQUAL", {"entityValue": {}}))))
        self.assertFalse(diff.entity_operand(aggregation(on("LESS_THAN", {"integerValue": "1"}))))
        self.assertFalse(diff.entity_operand({"method": "POST", "path": "/v1/projects/{project}:runQuery",
                                              "body": {"query": {"filter": on("LESS_THAN", {"entityValue": {}})}}}))

        order = lambda *names: [{"property": {"name": n}} for n in names]  # noqa: E731
        cursor_query = lambda **q: {"method": "POST", "path": "/v1/projects/{project}:runQuery",  # noqa: E731
                                    "body": {"query": {"kind": [{"name": "K"}], **q}}}
        self.assertTrue(diff.cursor_after_the_key(cursor_query(order=order("__key__", "e", "a"), startCursor="c")))
        self.assertTrue(diff.cursor_after_the_key(cursor_query(order=order("__key__", "e"), endCursor="c",
                                                               filter=on("LESS_THAN", {"integerValue": "1"}))))
        self.assertTrue(diff.cursor_after_the_key(cursor_query(order=order("__key__", "a"), startCursor="c",
                                                               projection=[{"property": {"name": "tags"}}])))
        self.assertFalse(diff.cursor_after_the_key(cursor_query(order=order("__key__", "e"), startCursor="c")))
        self.assertFalse(diff.cursor_after_the_key(cursor_query(order=order("e", "__key__", "a"), startCursor="c")))
        self.assertFalse(diff.cursor_after_the_key(cursor_query(order=order("__key__", "e", "a"))))

        begin = {"method": "POST", "path": "/v1/projects/{project}:beginTransaction", "raw": '{"projectId": "other"}'}
        self.assertTrue(diff.other_body_project(begin))
        self.assertTrue(diff.other_body_project({**begin, "raw": '{"projectId": "{project}x"}'}))
        self.assertFalse(diff.other_body_project({**begin, "raw": '{"projectId": "{project}"}'}))
        self.assertFalse(diff.other_body_project({**begin, "raw": '{"projectId": ""}'}))
        self.assertFalse(diff.other_body_project({**begin, "raw": '{"databaseId": "other"}'}))
        self.assertFalse(diff.other_body_project({**begin, "path": BASE + ":beginTransaction"}))

        group = {"from": [{"collectionId": "q", "allDescendants": True}]}
        self.assertTrue(diff.collection_group({**query, "body": {"structuredQuery": group}}))
        self.assertTrue(diff.collection_group({**query, "body": {"structuredAggregationQuery": {"structuredQuery": group}}}))
        self.assertFalse(diff.collection_group({**query, "body": {"structuredQuery": {"from": [{"collectionId": "q"}]}}}))
        self.assertFalse(diff.collection_group({**query, "body": {"structuredAggregationQuery": {}}}))

    def test_a_request_rule_matches_on_the_step_that_differs(self):
        rule = {"id": "r", "exception": "D001", "why": "w", "official": {"status": 404},
                "request": {"method": "^POST$", "path": ":commit$", "headers": {"content-type": "(?i)^text/"}}}
        rules = diff.Exceptions([rule])
        posted = {"method": "POST", "path": BASE + ":commit", "headers": {"Content-Type": "TEXT/plain"}, "raw": "{}"}
        case = {"steps": [patch({}), posted]}
        found = diff.compare(case, [answer(200, "{}"), answer(404, "Not Found", None)], [answer(200, "{}"), answer(200, "{}")])
        self.assertEqual(found["step"], 1)
        self.assertEqual(rules.match("malformed", found, case)["id"], "r")
        # Not without the case, nor for another step, method, path or content type.
        self.assertIsNone(rules.match("malformed", found))
        for other in ({**posted, "headers": {}}, {**posted, "method": "PUT"}, {**posted, "path": BASE + ":batchGet"}):
            self.assertIsNone(rules.match("malformed", found, {"steps": [patch({}), other]}), other)
        self.assertIsNone(rules.match("malformed", {**found, "step": 0}, case))

    def test_a_batch_denied_where_no_statement_matches(self):
        """D019: the official emulator lists what it checked before; hidane, what the denied one answers."""
        rules = diff.Exceptions.load()
        batch = {"method": "POST", "path": BASE + ":batchGet", "headers": {}, "body": {"documents": ["c/a", "d/x"]}}
        found = {"kind": "message", "verb": ":batchGet", "detail": "", "step": 0,
                 "official": {**DENIAL, "message": "\nfalse for 'get' @ L5"},
                 "hidane": {**DENIAL, "message": "No matching allow statements"}}
        self.assertEqual(rules.match("rules", found, {"steps": [batch]})["id"], "rules-denial-unmatched-after-others")
        one = {**batch, "body": {"documents": ["d/x"]}}
        self.assertIsNone(rules.match("rules", found, {"steps": [one]}))

    def test_an_earlier_rule_needs_an_earlier_step_and_its_answer(self):
        rule = {"id": "e", "exception": "D001", "why": "w", "official": {"status": 409},
                "earlier": {"path": ":runQuery$", "predicates": ["in-transaction"], "status": 400}}
        rules = diff.Exceptions([rule])
        query = {"method": "POST", "path": BASE + ":runQuery", "body": {"newTransaction": {"readWrite": {}}}}
        case = {"steps": [query, patch({})]}
        found = diff.compare(case, [error(400, "limit is negative"), error(409, "lock", "ABORTED")],
                             [error(400, "limit is negative"), answer(200, "{}")])
        self.assertEqual(rules.match("transactions", found, case, [error(400, "x"), error(409, "y")])["id"], "e")
        self.assertIsNone(rules.match("transactions", found, case, [answer(200, "[]"), error(409, "y")]))
        self.assertIsNone(rules.match("transactions", found, case))
        self.assertIsNone(rules.match("transactions", found, {"steps": [patch({}), patch({})]}, [error(400, "x")] * 2))

    def test_a_failed_query_in_a_named_transaction(self):
        """D085: a query that names a read-write transaction and fails keeps its lock on the
        official emulator."""
        rules = diff.Exceptions.load()
        begin = {"method": "POST", "path": BASE + ":batchGet", "body": {"newTransaction": {"readWrite": {}}}}
        query = {"method": "POST", "path": BASE + ":runQuery", "body": {"structuredQuery": {"from": [{"collectionId": "q"}], "limit": -1}, "transaction": "{tx}"}}
        write = {"method": "POST", "path": BASE + ":commit", "body": {"writes": [{"update": {"name": "q/zz"}}]}}
        case = {"steps": [begin, query, write]}
        official = [answer(200, "[]"), error(400, "limit is negative"), error(409, "Transaction lock timeout.", "ABORTED")]
        found = diff.compare(case, official, [answer(200, "[]"), error(400, "limit is negative"), answer(200, "{}")])
        self.assertEqual(rules.match("transactions", found, case, official)["id"], "failed-query-lock")
        untransacted = {**query, "body": {"structuredQuery": {"from": [{"collectionId": "q"}], "limit": -1}}}
        self.assertIsNone(rules.match("transactions", found, {"steps": [begin, untransacted, write]}, official))

    def test_a_write_its_client_gave_up_on(self):
        """D026: a Datastore write whose client gave up waiting for a lock goes through a moment
        after the holder ends on the official emulator, so a read right after can miss it."""
        rules = diff.Exceptions.load()
        key = {"path": [{"kind": "K", "name": "a"}]}
        step = lambda verb, body: {"method": "POST", "path": "/v1/projects/{project}:" + verb, "body": body}
        commit = step("commit", {"mode": "NON_TRANSACTIONAL", "mutations": [{"upsert": {"key": key}}]})
        case = {"steps": [commit, step("rollback", {"transaction": "{tx}"}), step("lookup", {"keys": [key]})]}
        gone = {"status": None, "error": "TimeoutError"}
        missing = answer(200, '{"missing": [{"entity": {"key": {}}}]}')
        there = answer(200, '{"found": [{"entity": {"key": {}}}]}')
        official = [gone, answer(200, "{}"), missing]
        found = diff.compare(case, official, [gone, answer(200, "{}"), there])
        self.assertEqual(rules.match("datastore-transactions", found, case, official)["id"], "write-applied-after-release")
        answered = [answer(200, "{}"), answer(200, "{}"), missing]
        found = diff.compare(case, answered, [answer(200, "{}"), answer(200, "{}"), there])
        self.assertIsNone(rules.match("datastore-transactions", found, case, answered))

    def test_an_aggregation_over_a_collection_group_under_a_document(self):
        """D088: the official emulator wants an index for a sum over a collection group under a
        document; hidane runs it."""
        rules = diff.Exceptions.load()
        aggregation = lambda selector: {"method": "POST", "path": BASE + "/q/d0:runAggregationQuery", "body": {
            "structuredAggregationQuery": {"structuredQuery": {"from": [selector]}, "aggregations": [{"sum": {"field": {"fieldPath": "a"}}}]}}}
        group = aggregation({"collectionId": "q", "allDescendants": True})
        official = [error(400, "This query requires an index that has fields [a] after __name__ and Firestore does not "
                               "currently support such an index. Please try the query again with additional ordering on those fields.")]
        case = {"steps": [group]}
        found = diff.compare(case, official, [answer(200, "[]")])
        self.assertEqual(rules.match("queries", found, case, official)["id"], "collection-group-under-document-index")
        collection = {"steps": [aggregation({"collectionId": "q"})]}
        self.assertIsNone(rules.match("queries", found, collection, official))

    def test_check_finds_bad_request_rules(self):
        bad = diff.Exceptions([{"id": "bad", "exception": "D008", "why": "w", "official": {"status": 1},
                                "request": {"path": "(", "headers": {"x": "["}, "predicates": ["nope"], "status": 400},
                                "earlier": {"method": "(", "extra": 1}},
                               {"id": "shape", "exception": "D008", "why": "w", "official": {"status": 1},
                                "request": [], "earlier": {"headers": [], "predicates": "lone-surrogate"}}])
        problems = "\n".join(bad.check({"D008"}))
        for expected in ("`request.path`", "`request.headers.x`", "unknown predicate 'nope'",
                         "unknown keys in `request`: ['status']", "`earlier.method`", "unknown keys in `earlier`: ['extra']",
                         "`request` must be an object", "`earlier.headers` must map", "`earlier.predicates` must be a list"):
            self.assertIn(expected, problems)

    def test_the_rules_in_use_accept_what_their_entries_say_and_not_the_bugs_next_to_them(self):
        rules = diff.Exceptions.load()

        def accepted(case, official, hidane):
            found = diff.compare(case, official, hidane)
            rule = rules.match(case["area"], found, case, official)
            return rule and rule["exception"]

        refused = error(400, PAYLOAD)
        stored = answer(200, '{"name": "x"}')
        # D083 and D084: the same answers, a lone surrogate or a lenient time against a number
        # given as a string (a bug).
        for value, entry in (({"stringValue": "\ud800"}, "D083"), ({"timestampValue": "2020-13-01T00:00:00Z"}, "D084"),
                             ({"stringValue": 1}, None), ({"timestampValue": "2020-12-01T00:00:00Z"}, None)):
            case = {"area": "malformed", "steps": [patch({"v": value})]}
            self.assertEqual(accepted(case, [stored], [refused]), entry, value)
        # D085: a lock timeout after a failed query that began a read-write transaction; not
        # after one that succeeded (a lock hidane would miss), nor without one.
        query = {"method": "POST", "path": BASE + ":runQuery", "headers": {},
                 "body": {"structuredQuery": {"from": [{"collectionId": "c"}], "limit": -1}, "newTransaction": {"readWrite": {}}}}
        write = patch({"a": {"integerValue": "1"}})
        timeout = error(409, "Transaction lock timeout.", "ABORTED")
        for first, entry in ((error(400, "limit is negative"), "D085"), (answer(200, "[]"), None)):
            case = {"area": "transactions", "steps": [query, write]}
            self.assertEqual(accepted(case, [first, timeout], [first, stored]), entry)
        self.assertIsNone(accepted({"area": "transactions", "steps": [write]}, [timeout], [stored]))
        # D086 and D087: a bare 404 for a form-encoded POST and for a long request line, not for
        # other requests.
        bare = answer(404, "Not Found", None)
        form = {"method": "POST", "path": BASE + ":commit", "headers": {"Content-Type": "application/x-www-form-urlencoded"},
                "raw": "{}"}
        self.assertEqual(accepted({"area": "malformed", "steps": [form]}, [bare], [answer(200, "{}")]), "D086")
        self.assertIsNone(accepted({"area": "malformed", "steps": [{**form, "headers": {}}]}, [bare], [answer(200, "{}")]))
        long = {"method": "GET", "path": BASE + "/c/" + "%C3%A9" * 751, "headers": {}}
        self.assertEqual(accepted({"area": "malformed", "steps": [long]}, [bare], [error(400, "x")]), "D087")
        self.assertIsNone(accepted({"area": "malformed", "steps": [{**long, "path": BASE + "/c/d"}]}, [bare],
                                   [error(400, "x")]))
        # D088: a collection group query under a document.
        group = {"method": "POST", "path": BASE + "/c/d:runQuery", "headers": {},
                 "body": {"structuredQuery": {"from": [{"collectionId": "q", "allDescendants": True}]}}}
        refusal = error(400, "collection group queries are only allowed if they're at the root or they're an ascending "
                             "primary key scan", "FAILED_PRECONDITION")
        self.assertEqual(accepted({"area": "queries", "steps": [group]}, [refusal], [answer(200, "[]")]), "D088")
        self.assertIsNone(accepted({"area": "queries", "steps": [{**group, "path": BASE + ":runQuery"}]}, [refusal],
                                   [answer(200, "[]")]))


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


class Minimising(unittest.TestCase):
    def test_a_smaller_case_that_a_rule_accepts_does_not_reproduce(self):
        # The same signature (one message template), but a rule accepts the smaller case's
        # wording: minimising to it would report a difference the run does not report.
        rule = {"id": "x", "exception": "D008", "why": "w", "kinds": ["message"],
                "hidane": {"message": "accepted"}}
        args = argparse.Namespace(mode="firestore-native", areas="", minutes=0, official="o", hidane="h", timeout=1)
        session = fuzz.Session(args, diff.Exceptions([rule]), Path("."))

        def both(case):
            word = "accepted" if len(case["steps"]) == 1 else "refused"
            return [error(400, "bad 'x'")], [error(400, f"bad '{word}'")]
        session.both = both
        big, small = {"steps": [STEP, STEP]}, {"steps": [STEP]}
        found = diff.compare(big, *both(big))
        self.assertEqual(diff.compare(small, *both(small))["signature"], found["signature"])
        test = session.reproduces("batches", found["signature"])
        self.assertTrue(test(big))
        self.assertFalse(test(small))
        self.assertFalse(session.unexplained("batches", found["signature"], None))

    def test_one_signature_accepted_for_one_request_and_not_for_another(self):
        # An official 200 against `Payload isn't valid for request.`: a lone surrogate (D083)
        # and a number given as a string (a bug) have one signature, and are two entries.
        rule = {"id": "lone", "exception": "D083", "why": "w", "hidane": {"message": "^Payload"},
                "request": {"predicates": ["lone-surrogate"]}}
        with tempfile.TemporaryDirectory() as out:
            args = argparse.Namespace(mode="firestore-native", areas="", minutes=0, official="o", hidane="h", timeout=1, minimize_seconds=5,
                                      minimize_budget=5, official_version=("1.22.0", "0" * 64))
            session = fuzz.Session(args, diff.Exceptions([rule]), Path(out))
            session.both = lambda case: ([answer(200, "{}")], [error(400, PAYLOAD)])
            cases = [{"name": f"case {n}", "area": "malformed", "seed": 1, "index": n, "steps": [patch({"v": value})]}
                     for n, value in enumerate([{"stringValue": "\ud800"}, {"stringValue": 1}, {"stringValue": "\udc00"}])]
            for case in cases:
                official, hidane = session.both(case)
                session.record(case, diff.compare(case, official, hidane), official)
            entries = sorted(session.signatures.values(), key=lambda e: e["outcome"])
            self.assertEqual([(e["outcome"], e["occurrences"]) for e in entries], [("accepted", 2), ("unexplained", 1)])
            self.assertEqual(entries[0]["signature"], entries[1]["signature"])
            self.assertEqual(entries[1]["found"]["case"], 1)
            self.assertEqual(session.by_area["malformed"]["accepted"], 2)


class FakeEmulator(BaseHTTPRequestHandler):
    """Answers like a tiny emulator; the `hidane` one refuses less than the `official` one. In
    Firestore Native mode unless `datastore`, which refuses the Datastore API."""
    lenient = False
    datastore = False

    def do_GET(self):
        self.reply(200, "Ok" if self.path == "/" else '{\n  "name": "x"\n}\n')

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("Content-Length") or 0)).decode()
        if re.match(r"/v1/projects/[^/]*:", self.path) and not self.datastore:
            self.reply(400, ERROR % (400, "Database mode not set to Datastore Mode.", "FAILED_PRECONDITION"))
        elif "NaN" in body and not self.lenient:
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


class DatastoreFake(FakeEmulator):
    datastore = True


class DatastoreLenient(DatastoreFake):
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


class OutOfPorts(unittest.TestCase):
    def test_a_client_out_of_local_ports_waits_for_them(self):
        import errno
        import urllib.error
        import urllib.request

        import harness

        real = urllib.request.urlopen
        failures = [urllib.error.URLError(OSError(errno.EADDRNOTAVAIL, "Can't assign requested address"))] * 2

        def urlopen(*args, **kwargs):
            if failures:
                raise failures.pop()
            return real(*args, **kwargs)

        with serving(FakeEmulator) as host, mock.patch("urllib.request.urlopen", urlopen), \
                mock.patch("time.sleep") as sleep:
            self.assertEqual(harness.send(host, "GET", "/")[0], 200)
        self.assertEqual(sleep.call_count, 2)

    def test_other_connection_errors_are_not_retried(self):
        import harness

        with serving(FakeEmulator) as host:
            port = int(host.split(":")[1])
        with self.assertRaises(OSError):
            harness.send(f"127.0.0.1:{port}", "GET", "/", timeout=2)


class EndToEnd(unittest.TestCase):
    def test_a_run_writes_minimised_differences_and_a_summary(self):
        with serving(FakeEmulator) as official, serving(Lenient) as hidane, tempfile.TemporaryDirectory() as out:
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                code = fuzz.main(["run", "--official", official, "--hidane", hidane, "--seed", "4", "--cases", "60",
                                  "--areas", "batches,transforms,documents", "--minimize-budget", "60", "--out", out,
                                  "--hidane-build", "0.4.0:" + COMMIT])
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
            # The build fuzzed, next to the official emulator's version.
            build = {"version": "0.4.0", "commit": COMMIT}
            self.assertEqual((summary["hidane"], run["hidane"]), (build, build))
            # The entry has the shape docs/compatibility.json's runs take.
            import corpus  # tools/oracle, which diff.py puts on the path
            schema = json.loads((diff.ROOT / "docs" / "compatibility.schema.json").read_text())
            self.assertEqual(corpus.problems(run, {"$ref": "#/$defs/fuzzing_run"}, schema), [])
            self.assertTrue(corpus.problems(dict(run, hidane={"version": "0.4.0", "commit": "abc"}),
                                            {"$ref": "#/$defs/fuzzing_run"}, schema))
            self.assertIn("Differential fuzzing, seed 4", Path(out, "summary.md").read_text())
            self.assertIn(f"and hidane 0.4.0 (commit {COMMIT[:12]})", Path(out, "summary.md").read_text())
            self.assertIn("| batches |", stdout.getvalue())

    def test_a_run_without_its_build_says_unknown_and_a_bad_build_is_refused(self):
        unknown = {"version": "unknown", "commit": "unknown"}
        with serving(FakeEmulator) as official, serving(Lenient) as hidane, tempfile.TemporaryDirectory() as out:
            with contextlib.redirect_stdout(io.StringIO()):
                code = fuzz.main(["run", "--official", official, "--hidane", hidane, "--seed", "4", "--cases", "1",
                                  "--out", out])
            self.assertEqual(code, 0)
            self.assertEqual(json.loads(Path(out, "summary.json").read_text())["hidane"], unknown)
            self.assertEqual(json.loads(Path(out, "run.json").read_text())["hidane"], unknown)
            self.assertIn("and hidane (build unknown) in", Path(out, "summary.md").read_text())
        for bad in ("0.4.0", "0.4.0:" + COMMIT[:12], "0.4.0:" + COMMIT.upper(), "v0.4.0:" + COMMIT,
                    "0.4.0 :" + COMMIT, ":" + COMMIT):
            with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
                fuzz.main(["run", "--official", "x", "--hidane", "y", "--seed", "1", "--out", "z",
                           "--hidane-build", bad])
        self.assertEqual(fuzz.hidane_build("0.5.0-rc.1:" + COMMIT), {"version": "0.5.0-rc.1", "commit": COMMIT})

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

    def test_a_run_in_datastore_mode(self):
        with serving(DatastoreFake) as official, serving(DatastoreLenient) as hidane, \
                tempfile.TemporaryDirectory() as out:
            with contextlib.redirect_stdout(io.StringIO()):
                code = fuzz.main(["run", "--official", official, "--hidane", hidane, "--seed", "2", "--cases", "40",
                                  "--mode", "datastore-mode", "--minimize-budget", "30", "--out", out])
            self.assertEqual(code, 0)
            summary = json.loads(Path(out, "summary.json").read_text())
            self.assertEqual((summary["mode"], set(summary["by_area"])), ("datastore-mode", set(gen.MODES["datastore-mode"])))
            # Doubles as NaN are refused by one fake only: a difference of Datastore requests.
            self.assertTrue(any(d["verb"].startswith("datastore:") for d in summary["differences"]))
            run = json.loads(Path(out, "run.json").read_text())
            self.assertTrue(run["name"].startswith("REST in Datastore mode, seed 2"))
            self.assertTrue(run["target"].startswith("REST requests to both emulators in Datastore mode: datastore-"))
            self.assertEqual(run["mode"], "datastore-mode")
            self.assertIn("Differential fuzzing in Datastore mode, seed 2", Path(out, "summary.md").read_text())

    def test_a_run_needs_both_emulators_in_its_mode_and_areas_of_that_mode(self):
        with serving(FakeEmulator) as official, serving(DatastoreLenient) as hidane, \
                tempfile.TemporaryDirectory() as out:
            for mode, message in (("datastore-mode", "the official emulator runs in firestore-native"),
                                  ("firestore-native", "the hidane emulator runs in datastore-mode")):
                stderr = io.StringIO()
                with contextlib.redirect_stderr(stderr):
                    code = fuzz.main(["run", "--official", official, "--hidane", hidane, "--seed", "1", "--cases", "1",
                                      "--mode", mode, "--out", out])
                self.assertEqual(code, 1)
                self.assertIn(message, stderr.getvalue())
        for mode, area in (("firestore-native", "datastore-gql"), ("datastore-mode", "queries")):
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(fuzz.main(["show", "--seed", "1", "--case", "0", "--mode", mode, "--areas", area]), 1)
            self.assertIn(f"unknown area {area!r} in {mode}", stderr.getvalue())

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
