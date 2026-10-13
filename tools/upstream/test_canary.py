"""Tests of canary.py, offline: recordings, cargo and GitHub's API are stand-ins.

Run from the repository root:
    python3 -m unittest discover -s tools/upstream -p "test_*.py"
"""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import canary
import watch

SHA = "a" * 64
FIXTURE_Q = "crates/hidane/tests/fixtures/queries.json"
FIXTURE_V = "crates/hidane-core/tests/fixtures/value_order.json"

CARGO_OK = """\
     Running tests/queries.rs (target/debug/deps/queries-0123)

running 1 test
test replay ... ok
"""
CARGO_FAILED = """\
     Running unittests src/lib.rs (target/debug/deps/hidane-abcd)
test store::tests::put ... ok
     Running tests/queries.rs (target/debug/deps/queries-0123)

running 2 tests
test replay ... FAILED
test cursors ... ok
     Running tests/rest.rs (target/debug/deps/rest-4567)
test replay ... FAILED
   Doc-tests hidane_core
test src/lib.rs - Value (line 3) ... FAILED
"""


class Tree:
    """A repository with two fixtures, and the canary's artifacts next to it."""

    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "repo"
        self.artifacts = Path(self.tmp.name) / "artifacts"
        for fixture in (FIXTURE_Q, FIXTURE_V):
            (self.root / fixture).parent.mkdir(parents=True, exist_ok=True)
            (self.root / fixture).write_text('{"committed": true}\n')

    def recording(self, slug, recorder, fixture, result, data=None, version="1.23.0"):
        out = self.artifacts / f"canary-{slug}-{recorder}" / "out"
        out.mkdir(parents=True, exist_ok=True)
        (out / "result.json").write_text(json.dumps(
            {"recorder": recorder, "fixture": fixture, "version": version, "result": result}))
        if data is not None:
            (out / f"{recorder}.json").write_text(data)

    def replayed(self, slug, **data):
        out = self.artifacts / f"canary-{slug}-hidane"
        out.mkdir(parents=True, exist_ok=True)
        base = {"changed": [], "tests": "passed", "failures": [], "regressions": [],
                "preexisting": []}
        (out / "replay.json").write_text(json.dumps(dict(base, **data)))

    def close(self):
        self.tmp.cleanup()


def upstream(candidates=(), packages=()):
    return {
        "packages": list(packages),
        "emulator": {
            "watched": {"name": "cloud-firestore-emulator", "version": "1.22.0", "sha256": "b" * 64,
                        "ui": "1.15.0"},
            "candidates": list(candidates)},
    }


def candidate(version="1.23.0", sha=SHA, channel="stable"):
    return {"key": f"emulator:{version}:{sha[:12]}", "slug": f"v{version}-{sha[:12]}",
            "version": version, "sha256": sha, "url": watch.JAR_URL.format(version=version),
            "url_ok": True, "channel": channel, "channels": [channel],
            "pinned_by": ["firebase-tools 15.34.0"]}


class Replay(unittest.TestCase):
    def setUp(self):
        self.tree = Tree()
        self.addCleanup(self.tree.close)
        self.slug = "v1.23.0-aaaaaaaaaaaa"

    def runner(self, outputs):
        """Answers each cargo run with the next output, and keeps what the fixture held."""
        seen = []

        def run(command):
            self.assertEqual(command[:2], ["cargo", "test"])
            seen.append((self.tree.root / FIXTURE_Q).read_text())
            code, text = outputs.pop(0)
            return subprocess.CompletedProcess(command, code, stdout=text)
        return run, seen

    def test_failures(self):
        self.assertEqual(canary.failures(CARGO_FAILED), [
            "hidane_core::src/lib.rs - Value (line 3)", "tests/queries.rs::replay",
            "tests/rest.rs::replay"])
        self.assertEqual(canary.failures(CARGO_OK), [])
        coloured = ("\x1b[1m\x1b[92m     Running\x1b[0m tests/queries.rs (target/debug/deps/q)\n"
                    "test replay ... \x1b[31mFAILED\x1b[0m\n")
        self.assertEqual(canary.failures(coloured), ["tests/queries.rs::replay"])

    def test_identical_recordings_run_the_tests_once(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "identical", '{"committed": true}\n')
        run, seen = self.runner([(0, CARGO_OK)])
        result, _ = canary.replay(self.tree.artifacts, run, self.tree.root)
        self.assertEqual(result, {"changed": [], "tests": "passed", "failures": [],
                                  "regressions": [], "preexisting": []})
        self.assertEqual(len(seen), 1)

    def test_a_different_recording_takes_the_fixture_place(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "different", '{"new": 1}\n')
        self.tree.recording(self.slug, "value_order", FIXTURE_V, "unstable", '{"x": 1}\n')
        run, seen = self.runner([(101, CARGO_FAILED), (101, "test replay ... FAILED\n")])
        result, log = canary.replay(self.tree.artifacts, run, self.tree.root)
        # First with the new recording, then again with the committed one.
        self.assertEqual(seen, ['{"new": 1}\n', '{"committed": true}\n'])
        self.assertEqual(result["changed"], [FIXTURE_Q])
        self.assertEqual(result["tests"], "failed")
        self.assertEqual(result["regressions"], [
            "hidane_core::src/lib.rs - Value (line 3)", "tests/queries.rs::replay",
            "tests/rest.rs::replay"])
        self.assertEqual(result["preexisting"], [])
        self.assertIn("Doc-tests", log)
        # The fixtures are as committed afterwards, the unstable one untouched.
        self.assertEqual((self.tree.root / FIXTURE_Q).read_text(), '{"committed": true}\n')
        self.assertEqual((self.tree.root / FIXTURE_V).read_text(), '{"committed": true}\n')

    def test_a_failure_with_the_committed_fixtures_is_not_a_regression(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "different", '{"new": 1}\n')
        run, _ = self.runner([(101, CARGO_FAILED), (101, CARGO_FAILED)])
        result, _ = canary.replay(self.tree.artifacts, run, self.tree.root)
        self.assertEqual(result["regressions"], [])
        self.assertEqual(len(result["preexisting"]), 3)

    def test_a_build_failure_is_an_error(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "identical")
        run, _ = self.runner([(101, "error[E0425]: cannot find value\n")])
        result, _ = canary.replay(self.tree.artifacts, run, self.tree.root)
        self.assertEqual(result["tests"], "error")

    def test_results_are_checked(self):
        for bad in ({"recorder": "../x", "fixture": FIXTURE_Q, "result": "identical"},
                    {"recorder": "queries", "fixture": "/etc/passwd", "result": "identical"},
                    {"recorder": "queries", "fixture": "crates/x/tests/fixtures/none.json",
                     "result": "identical"},
                    {"recorder": "queries", "fixture": FIXTURE_Q, "result": "fine"}):
            out = self.tree.artifacts / "bad" / "out"
            out.mkdir(parents=True, exist_ok=True)
            (out / "result.json").write_text(json.dumps(bad))
            with self.assertRaises(canary.Stop):
                canary.recorder_results(self.tree.artifacts, self.tree.root)

    def test_a_missing_recording_is_refused(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "different")
        with self.assertRaises(canary.Stop):
            canary.replay(self.tree.artifacts, self.runner([])[0], self.tree.root)


class Report(unittest.TestCase):
    def setUp(self):
        self.tree = Tree()
        self.addCleanup(self.tree.close)
        self.slug = "v1.23.0-aaaaaaaaaaaa"

    def report(self, **kwargs):
        data = canary.report(upstream(**kwargs), self.tree.artifacts, self.tree.root)
        json.dumps(data)  # it serializes
        return data

    def test_nothing_new(self):
        data = self.report()
        self.assertEqual((data["emulators"], data["sdks"], data["findings"]), ([], [], []))
        self.assertTrue(data["conclusive"])
        self.assertIn("No new official emulator", canary.summary(data))

    def test_same(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "identical")
        self.tree.recording(self.slug, "value_order", FIXTURE_V, "identical")
        self.tree.replayed(self.slug)
        data = self.report(candidates=[candidate()])
        [entry] = data["emulators"]
        self.assertEqual(entry["verdict"], "same")
        self.assertEqual((entry["fixtures_recorded"], entry["fixtures_total"]), (2, 2))
        self.assertEqual(data["findings"], [])
        self.assertTrue(data["conclusive"])
        self.assertIn("No confirmed difference: no issue.", canary.summary(data))

    def test_different(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "different")
        self.tree.recording(self.slug, "value_order", FIXTURE_V, "identical")
        self.tree.replayed(self.slug, tests="failed", changed=[FIXTURE_Q],
                           failures=["tests/queries.rs::replay"],
                           regressions=["tests/queries.rs::replay"])
        data = self.report(candidates=[candidate()])
        self.assertEqual(data["emulators"][0]["verdict"], "different")
        [finding] = data["findings"]
        self.assertEqual(finding["key"], "emulator:1.23.0:aaaaaaaaaaaa")
        self.assertTrue(finding["body"].startswith(
            '<!-- hidane-upstream-canary key="emulator:1.23.0:aaaaaaaaaaaa" -->'))
        self.assertIn("**different**", finding["body"])
        self.assertIn("`tests/queries.rs::replay`", finding["body"])
        self.assertIn("v1.23.0 (stable)", finding["title"])

    def test_inconclusive(self):
        cases = [
            # A recorder that failed.
            [("queries", FIXTURE_Q, "error")],
            # Two recordings that disagree: not confirmed.
            [("queries", FIXTURE_Q, "unstable")],
        ]
        for recordings in cases:
            with self.subTest(recordings=recordings):
                tree = Tree()
                self.addCleanup(tree.close)
                for recorder, fixture, result in recordings:
                    tree.recording(self.slug, recorder, fixture, result)
                tree.replayed(self.slug)
                data = canary.report(upstream(candidates=[candidate()]), tree.artifacts, tree.root)
                self.assertEqual(data["emulators"][0]["verdict"], "inconclusive")
                self.assertFalse(data["conclusive"])
                self.assertEqual(data["findings"], [])

    def test_inconclusive_without_tests_or_recordings(self):
        data = self.report(candidates=[candidate()])
        self.assertEqual(data["emulators"][0]["verdict"], "inconclusive")
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "identical")
        data = self.report(candidates=[candidate()])
        self.assertEqual(data["emulators"][0]["hidane"]["tests"], "not-run")
        self.assertEqual(data["emulators"][0]["verdict"], "inconclusive")

    def test_a_recorder_without_a_result_is_missing(self):
        workflow = self.tree.root / canary.RECORDINGS_WORKFLOW
        workflow.parent.mkdir(parents=True)
        workflow.write_text("        include:\n          - recorder: queries\n"
                            "            fixture: x\n          - recorder: limits\n")
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "identical")
        self.tree.replayed(self.slug)
        data = self.report(candidates=[candidate()])
        self.assertEqual([(r["recorder"], r["result"]) for r in data["emulators"][0]["recordings"]],
                         [("queries", "identical"), ("limits", "missing")])
        self.assertEqual(data["emulators"][0]["verdict"], "inconclusive")

    def test_the_repository_names_its_recorders(self):
        self.assertEqual(canary.expected_recorders(), [
            "value_order", "queries", "aggregations", "rest", "limits", "rules_paths"])

    def test_a_recording_of_another_release_does_not_count(self):
        self.tree.recording(self.slug, "queries", FIXTURE_Q, "identical", version="1.22.0")
        self.tree.replayed(self.slug)
        data = self.report(candidates=[candidate()])
        self.assertEqual(data["emulators"][0]["recordings"][0]["result"], "error")

    def test_sdks_are_listed_as_not_run(self):
        packages = [
            {"id": "npm:firebase-functions", "ecosystem": "npm", "name": "firebase-functions",
             "watched": "6.6.0", "verified": True,
             "new": [{"channel": "stable", "version": "7.4.0"}]},
            {"id": "npm:firebase-admin", "ecosystem": "npm", "name": "firebase-admin",
             "watched": "14.5.0", "verified": False,
             "new": [{"channel": "stable", "version": "14.6.0"}]},
        ]
        data = self.report(packages=packages)
        self.assertEqual([(s["key"], s["result"]) for s in data["sdks"]], [
            ("npm:firebase-functions@7.4.0", "not-run"), ("npm:firebase-admin@14.6.0", "not-run")])
        self.assertIn("tools/oracle/functions/", data["sdks"][0]["reason"])
        self.assertIn("without being verified", data["sdks"][1]["reason"])
        self.assertEqual(data["findings"], [])
        self.assertIn("| npm: `firebase-functions` | `7.4.0` | stable | not-run",
                      canary.summary(data))

    def test_marker(self):
        self.assertEqual(canary.marker("npm:@google-cloud/firestore@9.4.0"),
                         '<!-- hidane-upstream-canary key="npm:@google-cloud/firestore@9.4.0" -->')
        for bad in ('x" --><script>', "emulator:1.0 0", ""):
            with self.assertRaises(canary.Stop):
                canary.marker(bad)


class FakeGitHub:
    """GitHub's REST API for issues and labels, in memory."""

    def __init__(self, issues=(), labels=("area:parity",)):
        self.issues = [dict(i) for i in issues]
        self.labels = set(labels)
        self.calls = []

    def call(self, method, path, body=None):
        self.calls.append((method, path))
        if method == "GET" and path.startswith("/labels/"):
            name = path[len("/labels/"):].replace("%3A", ":")
            return (200, {"name": name}) if name in self.labels else (404, None)
        if method == "POST" and path == "/labels":
            self.labels.add(body["name"])
            return 201, body
        if method == "GET" and path.startswith("/issues?"):
            page = int(path.rsplit("page=", 1)[1])
            return 200, (self.issues if page == 1 else [])
        if method == "POST" and path == "/issues":
            number = 100 + len(self.issues)
            self.issues.append(dict(body, number=number, state="open"))
            return 201, {"number": number}
        if method == "PATCH" and path.startswith("/issues/"):
            number = int(path.rsplit("/", 1)[1])
            issue = next(i for i in self.issues if i["number"] == number)
            issue.update(body)
            return 200, issue
        raise AssertionError(f"unexpected call {method} {path}")


def finding(key="emulator:1.23.0:aaaaaaaaaaaa", body="new body"):
    return {"key": key, "title": f"Upstream canary: {key}",
            "body": f"{canary.marker(key)}\n{body}\n"}


class Issues(unittest.TestCase):
    def test_opens_one(self):
        api = FakeGitHub(labels=())
        done = canary.file_issues([finding()], api, dry_run=False)
        self.assertEqual(done, ["opened #100: Upstream canary: emulator:1.23.0:aaaaaaaaaaaa"])
        self.assertEqual(api.issues[0]["labels"], ["upstream"])  # area:parity does not exist
        self.assertIn("upstream", api.labels)

    def test_updates_the_open_one(self):
        existing = {"number": 7, "state": "open", "body": finding(body="old body")["body"]}
        other = {"number": 8, "state": "open", "body": finding("emulator:1.24.0:bbbbbbbbbbbb")["body"]}
        api = FakeGitHub([existing, other, {"number": 9, "state": "open", "pull_request": {},
                                            "body": finding()["body"]}])
        done = canary.file_issues([finding()], api, dry_run=False)
        self.assertEqual(done, ["updated #7: Upstream canary: emulator:1.23.0:aaaaaaaaaaaa"])
        self.assertEqual(api.issues[0]["body"], finding()["body"])
        self.assertEqual(len(api.issues), 3)
        # Once it says the same, it is left alone.
        self.assertIn("already says this", canary.file_issues([finding()], api, False)[0])

    def test_a_closed_one_is_not_reopened_or_duplicated(self):
        api = FakeGitHub([{"number": 7, "state": "closed", "body": finding()["body"]}])
        done = canary.file_issues([finding(body="newer")], api, dry_run=False)
        self.assertEqual(done, ["#7 closed for this release; not reopened: "
                                "Upstream canary: emulator:1.23.0:aaaaaaaaaaaa"])
        self.assertEqual([c for c in api.calls if c[0] != "GET"], [])

    def test_dry_run_writes_nothing(self):
        api = FakeGitHub([{"number": 7, "state": "open", "body": finding(body="old")["body"]}])
        done = canary.file_issues([finding(), finding("emulator:1.24.0:bbbbbbbbbbbb")], api,
                                  dry_run=True)
        self.assertEqual(done, ["would update #7: Upstream canary: emulator:1.23.0:aaaaaaaaaaaa",
                                "would open: Upstream canary: emulator:1.24.0:bbbbbbbbbbbb"])
        self.assertEqual([c for c in api.calls if c[0] != "GET"], [])

    def test_no_finding(self):
        api = FakeGitHub()
        self.assertEqual(canary.file_issues([], api, dry_run=False), ["No finding: no issue."])
        self.assertEqual(api.calls, [])

    def test_api_settings_are_checked(self):
        with self.assertRaises(canary.Stop):
            canary.GitHub("not a repository", "token")
        with self.assertRaises(canary.Stop):
            canary.GitHub("hidane-dev/hidane", "token", "https://example.com")


if __name__ == "__main__":
    unittest.main()
