"""Offline tests of issues.py, against an in-memory stand-in for the GitHub API."""

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import issues  # noqa: E402


def entry(signature, kind="status", occurrences=1, outcome="unexplained", area="batches", verb=":commit", **extra):
    return {"signature": signature, "kind": kind, "area": area, "verb": verb, "detail": "x",
            "outcome": outcome, "occurrences": occurrences, "found": {"seed": 7, "case": 3, "step": 0},
            "official": {"status": 400, "code": "INVALID_ARGUMENT", "message": "Bad `x` <b>\n```"},
            "hidane": {"status": 200}, **extra}


class FakeGitHub:
    def __init__(self, issues=(), labels=("area:parity",)):
        self.issues = list(issues)
        self.labels = set(labels)
        self.calls = []

    def call(self, method, path, payload=None):
        self.calls.append((method, path, payload))
        if method == "GET" and path.startswith("/issues?"):
            page = int(path.rsplit("page=", 1)[1])
            return 200, self.issues[(page - 1) * 100:page * 100]
        if method == "GET" and path.startswith("/labels/"):
            return (200 if path[len("/labels/"):].replace("%3A", ":") in self.labels else 404), None
        if method == "POST" and path == "/labels":
            self.labels.add(payload["name"])
            return 201, {}
        if method == "POST" and path == "/issues":
            number = 100 + len(self.issues)
            self.issues.append({"number": number, "state": "open", **payload})
            return 201, {"number": number}
        if method == "PATCH":
            return 200, {}
        return 404, None


class Issues(unittest.TestCase):
    def test_only_fields_of_the_expected_form_are_used(self):
        self.assertEqual(issues.valid(entry("status-commit-0123456789ab"))["seed"], 7)
        for bad in (entry("status-commit-0123456789ab", outcome="accepted"), entry("x"),
                    entry("status-commit-0123456789ab", kind="other"), entry("status-commit-0123456789ab", verb="<img>"),
                    {**entry("status-commit-0123456789ab"), "official": {"status": "200"}},
                    {**entry("status-commit-0123456789ab"), "official": {"status": 400, "code": "bad code"}},
                    {**entry("status-commit-0123456789ab"), "found": {"seed": -1, "case": 0, "step": 0}}, {}):
            self.assertIsNone(issues.valid(bad), bad)

    def test_bodies_escape_what_the_emulators_said(self):
        f = issues.valid(entry("status-commit-0123456789ab"))
        text = issues.body(f, {"id": "42", "url": "https://github.com/o/r/actions/runs/42"}, "41")
        self.assertTrue(text.startswith('<!-- hidane-fuzz signature="status-commit-0123456789ab" first-run="41" -->\n'))
        self.assertIn("official: Bad 'x' ‹b›\\n'''", text)
        self.assertNotIn("<b>", text)
        self.assertEqual(text.count("```"), 4)  # the two code blocks the body opens and closes
        self.assertIn("python3 -I tools/fuzz/fuzz.py show --seed 7 --case 3", text)
        self.assertEqual(issues.title(f), "Differential fuzzing: status difference in :commit (status-commit-0123456789ab)")
        self.assertIn("run's `fuzz` artifact", text)

    def test_the_areas_are_the_generators(self):
        import gen
        self.assertEqual(issues.AREAS, {area for areas in gen.MODES.values() for area in areas})
        self.assertEqual(issues.DATASTORE_AREAS, set(gen.MODES["datastore-mode"]))

    def test_a_datastore_difference_names_its_mode_and_artifact(self):
        f = issues.valid(entry("status-datastore-commit-0123456789ab", area="datastore-entities",
                               verb="datastore:commit"))
        text = issues.body(f, {"id": "42", "url": "https://github.com/o/r/actions/runs/42"}, "41")
        self.assertIn("run's `fuzz-datastore` artifact", text)
        self.assertIn("python3 -I tools/fuzz/fuzz.py show --seed 7 --case 3 --mode datastore-mode", text)
        self.assertIn("--database-mode datastore-mode", text)

    def test_plan_updates_open_issues_leaves_closed_ones_and_caps_new_ones(self):
        findings = [issues.valid(entry(f"message-commit-00000000000{n}", kind="message", occurrences=n)) for n in range(4)]
        findings.append(issues.valid(entry("crash-commit-0123456789ab", kind="crash")))
        known = {"message-commit-000000000001": {"number": 5, "state": "open", "first_run": "1"},
                 "message-commit-000000000002": {"number": 6, "state": "closed", "first_run": "1"}}
        actions = [(f["signature"], what) for f, what, _ in issues.plan(findings, known, 2)]
        self.assertEqual(actions, [("crash-commit-0123456789ab", "open"), ("message-commit-000000000003", "open"),
                                   ("message-commit-000000000002", "closed"), ("message-commit-000000000001", "update"),
                                   ("message-commit-000000000000", "later")])

    def test_existing_reads_only_markers_at_the_start(self):
        api = FakeGitHub([
            {"number": 1, "state": "open", "body": '<!-- hidane-fuzz signature="data-commit-0123456789ab" first-run="9" -->\nx'},
            {"number": 2, "state": "open", "body": 'text\n<!-- hidane-fuzz signature="data-commit-ffffffffffff" first-run="9" -->'},
            {"number": 3, "state": "open", "pull_request": {}, "body": '<!-- hidane-fuzz signature="x" first-run="" -->'},
            {"number": 4, "state": "closed", "body": None},
        ])
        self.assertEqual(issues.existing(api), {"data-commit-0123456789ab": {"number": 1, "state": "open", "first_run": "9"}})

    def run_main(self, api, *args, findings=None):
        with tempfile.TemporaryDirectory() as out:
            Path(out, "summary.json").write_text(json.dumps({"differences": findings or [
                entry("status-commit-0123456789ab"), entry("data-commit-0123456789ab", kind="data"),
                entry("message-commit-0123456789ab", outcome="accepted"), entry("bad")]}))
            env = {"GITHUB_REPOSITORY": "o/r", "GITHUB_RUN_ID": "77", "GH_TOKEN": "t"}
            stdout = io.StringIO()
            with mock.patch.dict(os.environ, env), mock.patch.object(issues, "GitHub", lambda *a: api), \
                    contextlib.redirect_stdout(stdout):
                code = issues.main([out, *args])
            return code, stdout.getvalue()

    def test_dry_run_changes_nothing(self):
        api = FakeGitHub()
        code, out = self.run_main(api, "--dry-run")
        self.assertEqual(code, 0)
        self.assertEqual({method for method, _, _ in api.calls}, {"GET"})
        self.assertIn("`data-commit-0123456789ab`: would open an issue", out)
        self.assertIn("1 differences left out", out)

    def test_opens_then_updates(self):
        api = FakeGitHub()
        self.assertEqual(self.run_main(api)[0], 0)
        opened = [p for m, path, p in api.calls if m == "POST" and path == "/issues"]
        self.assertEqual(len(opened), 2)
        self.assertEqual(opened[0]["labels"], ["fuzz", "area:parity"])
        self.assertIn("fuzz", api.labels)
        self.assertIn('first-run="77"', opened[0]["body"])
        self.assertIn("https://github.com/o/r/actions/runs/77", opened[0]["body"])
        api.calls.clear()
        code, out = self.run_main(api)
        self.assertEqual(code, 0)
        self.assertEqual(sorted(m for m, _, _ in api.calls if m != "GET"), ["PATCH", "PATCH"])
        self.assertIn("updated #100", out)

    def test_refuses_other_apis_and_repositories(self):
        with self.assertRaises(issues.Stop):
            issues.GitHub("o/r", "t", "https://example.com")
        with self.assertRaises(issues.Stop):
            issues.GitHub("o/r/../x", "t")


if __name__ == "__main__":
    unittest.main()
