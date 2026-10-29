"""Tests of sdk_canary.py, offline: the emulators and the harness are stand-ins.

Run from the repository root:
    python3 -m unittest discover -s tools/upstream -p "test_*.py"
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path

import sdk_canary
import watch

# A harness: prints, for the emulator FIRESTORE_EMULATOR_HOST names, the next transcript of
# scenario.json ({"official": [...], "hidane": [...]}, one per run), or fails on "fail".
HARNESS = """\
import json, os, sys
from pathlib import Path
side = os.environ["FIRESTORE_EMULATOR_HOST"].split(".")[0]
count = Path(f"count-{side}")
n = int(count.read_text()) if count.exists() else 0
count.write_text(str(n + 1))
runs = json.loads(Path("scenario.json").read_text())[side]
out = runs[min(n, len(runs) - 1)]
if out == "fail":
    sys.exit(3)
print(out if isinstance(out, str) else json.dumps(out))
"""


class FakeEmulator:
    def __init__(self, side):
        self.side = side
        self.started = []

    def start(self, name):
        self.started.append(name)
        return (f"{self.side}.test:1", name)

    def stop(self, handle):
        pass


def repository(tmp, pinned="1.22.0"):
    """A repository with the watch list, a committed pair per unit and two Node scripts."""
    root = Path(tmp) / "repo"
    (root / "tools/upstream").mkdir(parents=True)
    (root / "tools/oracle").mkdir(parents=True)
    (root / "results").mkdir()
    data = json.loads((watch.ROOT / watch.WATCH).read_text())
    data["emulator"]["version"] = pinned
    (root / watch.WATCH).write_text(json.dumps(data))
    (root / "tools/oracle/sdk_documents.mjs").write_text(
        "import { Firestore } from '@google-cloud/firestore';\n")
    (root / "tools/oracle/sdk_web_writes.mjs").write_text(
        "import { initializeApp } from 'firebase/app';\nimport { x } from './local.mjs';\n")
    (root / "tools/oracle/sdk_export_import.mjs").write_text(
        "import { Firestore } from '@google-cloud/firestore';\n")
    return root


def commit_pair(root, transcript, official, hidane, pinned="1.22.0"):
    (root / "results" / f"{transcript}-official-v{pinned}.json").write_text(json.dumps(official))
    (root / "results" / f"{transcript}-hidane.json").write_text(json.dumps(hidane))


class Units(unittest.TestCase):
    def test_keys(self):
        self.assertEqual(sdk_canary.parse_key("npm:@google-cloud/firestore@9.4.0"),
                         ("npm", "@google-cloud/firestore", "9.4.0"))
        self.assertEqual(sdk_canary.parse_key("maven:com.google.cloud:google-cloud-firestore@3.50.0"),
                         ("maven", "com.google.cloud:google-cloud-firestore", "3.50.0"))
        for bad in ("npm:firebase-functions@7.4.0", "go:cloud.google.com/go/firestore@1.0; x",
                    "pypi:google-cloud-firestore", ""):
            with self.assertRaises(sdk_canary.Stop):
                sdk_canary.parse_key(bad)

    def test_plan(self):
        upstream = {"packages": [
            {"id": "go:cloud.google.com/go/firestore", "ecosystem": "go",
             "name": "cloud.google.com/go/firestore", "verified": True,
             "new": [{"channel": "stable", "version": "1.27.0"},
                     {"channel": "prerelease", "version": "1.28.0-rc.1"}]},
            {"id": "pypi:google-cloud-firestore", "ecosystem": "pypi",
             "name": "google-cloud-firestore", "verified": True,
             "new": [{"channel": "latest", "version": "2.35.0"},
                     {"channel": "prerelease", "version": "2.36.0b1", "requested": True}]},
            {"id": "npm:firebase-functions", "ecosystem": "npm", "name": "firebase-functions",
             "verified": True, "new": [{"channel": "stable", "version": "7.4.0"}]},
            {"id": "pypi:firebase-admin", "ecosystem": "pypi", "name": "firebase-admin",
             "verified": False, "new": [{"channel": "stable", "version": "7.8.0"}]},
        ], "emulator": {"candidates": []}}
        # A stable release by itself; a prerelease or a `latest` only when asked for by hand.
        self.assertEqual(sdk_canary.plan(upstream), [{
            "key": "go:cloud.google.com/go/firestore@1.27.0",
            "slug": "sdk-go-cloud.google.com-go-firestore-1.27.0",
            "name": "cloud.google.com/go/firestore", "version": "1.27.0", "harness": "go"}, {
            "key": "pypi:google-cloud-firestore@2.36.0b1",
            "slug": "sdk-pypi-google-cloud-firestore-2.36.0b1",
            "name": "google-cloud-firestore", "version": "2.36.0b1", "harness": "python"}])

    def test_units(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = repository(tmp)
            self.assertEqual(sdk_canary.units("node", "@google-cloud/firestore", root),
                             [("sdk_documents.mjs", "sdk-documents")])
            self.assertEqual(sdk_canary.units("node", "firebase", root),
                             [("sdk_web_writes.mjs", "sdk-web-writes")])
        self.assertEqual([t for _, t in sdk_canary.units("java", "x")],
                         ["sdk-java-documents", "sdk-java-queries", "sdk-java-transactions",
                          "sdk-java-listen"])

    def test_every_unit_of_the_repository_has_its_committed_pair(self):
        pinned = watch.load_watch()["emulator"]["version"]
        for ident, harness in sdk_canary.HARNESSES.items():
            name = ident.split(":", 1)[1]
            found = sdk_canary.units(harness, name)
            self.assertTrue(found, ident)
            for _, transcript in found:
                sdk_canary.committed(transcript, pinned)

    def test_node_imports(self):
        text = ("import { a } from 'firebase/firestore/lite';\nimport x from '@scope/pkg/sub';\n"
                "import y from './y.mjs';\nimport fs from 'node:fs';\n"
                "import p from '@firebase/rules-unit-testing/package.json' with { type: 'json' };\n")
        self.assertEqual(sdk_canary.node_imports(text),
                         ["firebase", "@scope/pkg", "@firebase/rules-unit-testing"])


class Compare(unittest.TestCase):
    def test_differences(self):
        a = [{"name": "x", "ok": {"n": 1, "s": "a"}}, {"name": "y"}]
        b = [{"name": "x", "ok": {"n": 1.0, "s": "a", "t": True}}]
        self.assertEqual(sdk_canary.differences(a, b), [
            ("$[0].ok.n", "1", "1.0"), ("$[0].ok.t", '"<absent>"', "true"),
            ("$[1]", '{"name": "y"}', '"<absent>"')])

    def test_unexplained(self):
        same = [{"ok": 1}]
        self.assertEqual(sdk_canary.unexplained((same, same), set()), [])
        committed = ([{"error": "key 1"}, {"a": 1}], [{"error": "path"}, {"a": 2}])
        allowed = set(sdk_canary.differences(*committed))
        # The same differences, or fewer of them: explained.
        self.assertEqual(sdk_canary.unexplained(committed, allowed), [])
        self.assertEqual(sdk_canary.unexplained(([{"error": "key 1"}, {"a": 1}],
                                                 [{"error": "path"}, {"a": 1}]), allowed), [])
        # Another value at a known path is not.
        self.assertEqual(sdk_canary.unexplained(([{"error": "key 1"}], [{"error": "other"}]),
                                                allowed),
                         [("$[0].error", '"key 1"', '"other"')])
        # Unless the path is one of the places explained whatever the values.
        self.assertEqual(sdk_canary.unexplained(([{"error": "key 1"}], [{"error": "other"}]),
                                                allowed, {"$[0].error"}), [])


class Run(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = repository(self.tmp.name)
        self.work = Path(self.tmp.name) / "work"
        self.out = Path(self.tmp.name) / "out"
        (self.work).mkdir()
        (self.work / "harness.py").write_text(HARNESS)
        self.official, self.hidane = FakeEmulator("official"), FakeEmulator("hidane")
        self.prepared = []
        self.fail_verified = False
        for section in sdk_canary.SECTIONS:
            commit_pair(self.root, f"sdk-go-{section}", [{"ok": 1}], [{"ok": 1}])

    def prepare(self, harness, name, version, work, logs, watched, root):
        self.assertEqual((harness, name), ("go", "cloud.google.com/go/firestore"))
        self.assertEqual(watched["npm:firebase"], "13.0.0")
        self.prepared.append((version, work.name))
        if version == "1.26.0" and self.fail_verified:
            raise sdk_canary.Stop("preparing the harness: `go get` failed")
        # Both releases run the same stand-in, which answers from scenario.json in turn.
        return lambda unit: ([sys.executable, "-I", str(self.work / "harness.py")], self.work)

    def scenario(self, official, hidane):
        """The transcripts each emulator gives, run after run (the last one repeats)."""
        for count in self.work.glob("count-*"):
            count.unlink()
        (self.work / "scenario.json").write_text(json.dumps({"official": official,
                                                             "hidane": hidane}))

    def run_release(self, version="1.27.0"):
        return sdk_canary.run_release(f"go:cloud.google.com/go/firestore@{version}",
                                      self.official, self.hidane, self.work, self.out, self.root,
                                      self.prepare)

    def results(self, result):
        return [(u["unit"], u["result"]) for u in result["units"]]

    def test_identical(self):
        self.scenario([[{"ok": 1}]], [[{"ok": 1}]])
        result = self.run_release()
        self.assertTrue(result["prepared"])
        self.assertEqual(result["verified"], "1.26.0")
        self.assertEqual(self.results(result), [(s, "identical") for s in sdk_canary.SECTIONS])
        self.assertFalse(result["units"][0]["official_changed"])
        self.assertTrue((self.out / "transcripts" / "sdk-go-documents-hidane.json").is_file())
        # One fresh emulator of each per unit, and the verified release never needed.
        self.assertEqual(len(self.official.started), 4)
        self.assertEqual(self.prepared, [("1.27.0", "release")])

    def test_documented_by_the_committed_pair(self):
        commit_pair(self.root, "sdk-go-documents", [{"error": "key 1"}], [{"error": "path"}])
        self.scenario([[{"error": "key 1"}, {"extra": True}]], [[{"error": "path"}, {"extra": True}]])
        unit = self.run_release()["units"][0]
        self.assertEqual((unit["result"], unit["differences"]), ("documented", 1))
        self.assertTrue(unit["official_changed"])
        self.assertNotIn("explained_by_verified_run", unit)

    def test_documented_by_the_verified_release_on_the_runner(self):
        # A step that depends on timing: both releases give 4 against 3 here.
        self.scenario([[{"attempts": 4}]], [[{"attempts": 3}]])
        result = self.run_release()
        unit = result["units"][0]
        self.assertEqual((unit["result"], unit["explained_by_verified_run"]), ("documented", True))
        self.assertEqual(self.prepared, [("1.27.0", "release"), ("1.26.0", "verified")])
        self.assertEqual(self.official.started[:2],
                         ["sdk-go-documents", "sdk-go-documents.verified"])

    def test_documented_by_the_place_the_verified_release_differs(self):
        # The official emulator counts 4 attempts with the new release and 5 with the verified
        # one, on the same runner, against hidane's 3 (#414): the place is explained.
        self.scenario([[{"attempts": 4}], [{"attempts": 5}], [{"attempts": 4}]],
                      [[{"attempts": 3}]])
        unit = self.run_release()["units"][0]
        self.assertEqual((unit["result"], unit["explained_by_verified_run"]), ("documented", True))

    def test_another_place_is_not_explained_by_the_verified_release(self):
        self.scenario([[{"attempts": 4, "n": 2}], [{"attempts": 5, "n": 1}],
                       [{"attempts": 4, "n": 2}]],
                      [[{"attempts": 3, "n": 1}]])
        unit = self.run_release()["units"][0]
        self.assertEqual((unit["result"], unit["paths"]), ("different", ["$[0].n"]))
        self.assertFalse(unit["explained_by_verified_run"])

    def test_different_is_confirmed_by_a_second_run(self):
        # The new release differs, the verified one does not, the new one differs again.
        self.scenario([[{"ok": 1}]], [[{"ok": 2}], [{"ok": 1}], [{"ok": 2}]])
        unit = self.run_release()["units"][0]
        self.assertEqual((unit["result"], unit["paths"]), ("different", ["$[0].ok"]))
        self.assertFalse(unit["explained_by_verified_run"])
        self.assertEqual(self.official.started[:3], [
            "sdk-go-documents", "sdk-go-documents.verified", "sdk-go-documents.again"])
        self.assertTrue((self.out / "transcripts" / "sdk-go-documents.again-hidane.json").is_file())

    def test_unstable_when_the_second_run_differs(self):
        self.scenario([[{"ok": 1}]], [[{"ok": 2}], [{"ok": 1}], [{"ok": 1}]])
        self.assertEqual(self.run_release()["units"][0]["result"], "unstable")

    def test_the_verified_release_against_the_committed_pair_only(self):
        # Asked for the verified release itself: no run to compare with but the committed pair.
        self.scenario([[{"ok": 1}]], [[{"ok": 2}]])
        unit = self.run_release("1.26.0")["units"][0]
        self.assertEqual(unit["result"], "different")
        self.assertEqual(self.prepared, [("1.26.0", "release")])

    def test_errors(self):
        self.scenario(["fail"], [[{"ok": 1}]])
        unit = self.run_release()["units"][0]
        self.assertEqual((unit["result"], unit["why"]), ("error", "official: exited with 3"))
        self.scenario(["not json"], [[{"ok": 1}]])
        unit = self.run_release()["units"][0]
        self.assertEqual(unit["why"], "official: printed something that is not JSON")
        self.assertEqual((self.out / "logs" / "sdk-go-documents.official.stdout").read_text(),
                         "not json\n")

    def test_a_verified_release_that_cannot_be_installed(self):
        self.fail_verified = True
        self.scenario([[{"ok": 1}]], [[{"ok": 2}]])
        unit = self.run_release()["units"][0]
        self.assertEqual(unit["result"], "error")
        self.assertIn("the verified release", unit["why"])

    def test_a_harness_that_cannot_be_installed(self):
        def fails(*args):
            raise sdk_canary.Stop("preparing the harness: `go get` failed")
        result = sdk_canary.run_release("go:cloud.google.com/go/firestore@1.27.0", self.official,
                                        self.hidane, self.work, self.out, self.root, fails)
        self.assertEqual((result["prepared"], result["units"]), (False, []))
        self.assertIn("go get", result["error"])


if __name__ == "__main__":
    unittest.main()
