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
             "new": [{"channel": "prerelease", "version": "1.27.0-rc.1"}]},
            {"id": "npm:firebase-functions", "ecosystem": "npm", "name": "firebase-functions",
             "verified": True, "new": [{"channel": "stable", "version": "7.4.0"}]},
            {"id": "pypi:firebase-admin", "ecosystem": "pypi", "name": "firebase-admin",
             "verified": False, "new": [{"channel": "stable", "version": "7.8.0"}]},
        ], "emulator": {"candidates": []}}
        self.assertEqual(sdk_canary.plan(upstream), [{
            "key": "go:cloud.google.com/go/firestore@1.27.0-rc.1",
            "slug": "sdk-go-cloud.google.com-go-firestore-1.27.0-rc.1",
            "name": "cloud.google.com/go/firestore", "version": "1.27.0-rc.1", "harness": "go"}])

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

    def test_classify(self):
        same = [{"ok": 1}]
        self.assertEqual(sdk_canary.classify(same, same, same, same)[0], "identical")
        committed = ([{"error": "Datastore key 1"}], [{"error": "path a"}])
        new = ([{"error": "Datastore key 1"}], [{"error": "path a"}])
        self.assertEqual(sdk_canary.classify(*new, *committed)[0], "documented")
        other = ([{"error": "Datastore key 1"}], [{"error": "path b"}])
        verdict, found = sdk_canary.classify(*other, *committed)
        self.assertIsNone(verdict)
        self.assertEqual(found, [("$[0].error", '"Datastore key 1"', '"path b"')])


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
        for section in sdk_canary.SECTIONS:
            commit_pair(self.root, f"sdk-go-{section}", [{"ok": 1}], [{"ok": 1}])

    def prepare(self, harness, name, version, work, logs, watched, root):
        self.assertEqual((harness, name, version), ("go", "cloud.google.com/go/firestore",
                                                    "1.27.0"))
        self.assertEqual(watched["npm:firebase"], "13.0.0")
        return lambda unit: ([sys.executable, "-I", str(work / "harness.py")], work)

    def scenario(self, official, hidane):
        for count in self.work.glob("count-*"):
            count.unlink()
        (self.work / "scenario.json").write_text(json.dumps({"official": official,
                                                             "hidane": hidane}))

    def run_release(self):
        return sdk_canary.run_release("go:cloud.google.com/go/firestore@1.27.0", self.official,
                                      self.hidane, self.work, self.out, self.root, self.prepare)

    def results(self, result):
        return [(u["unit"], u["result"]) for u in result["units"]]

    def test_identical(self):
        self.scenario([[{"ok": 1}]], [[{"ok": 1}]])
        result = self.run_release()
        self.assertTrue(result["prepared"])
        self.assertEqual(self.results(result), [(s, "identical") for s in sdk_canary.SECTIONS])
        self.assertFalse(result["units"][0]["official_changed"])
        self.assertTrue((self.out / "transcripts" / "sdk-go-documents-hidane.json").is_file())
        # One fresh emulator of each per unit.
        self.assertEqual(len(self.official.started), 4)

    def test_documented_and_official_changed(self):
        commit_pair(self.root, "sdk-go-documents", [{"error": "key 1"}], [{"error": "path"}])
        self.scenario([[{"error": "key 1"}, {"extra": True}]], [[{"error": "path"}, {"extra": True}]])
        result = self.run_release()
        unit = result["units"][0]
        self.assertEqual((unit["result"], unit["differences"]), ("documented", 1))
        self.assertTrue(unit["official_changed"])

    def test_different_is_confirmed_by_a_second_run(self):
        self.scenario([[{"ok": 1}]], [[{"ok": 2}]])
        result = self.run_release()
        unit = result["units"][0]
        self.assertEqual((unit["result"], unit["paths"]), ("different", ["$[0].ok"]))
        self.assertEqual(self.official.started[:2], ["sdk-go-documents", "sdk-go-documents.again"])
        self.assertTrue((self.out / "transcripts" / "sdk-go-documents-hidane.again.json").is_file())

    def test_unstable_when_the_second_run_differs(self):
        self.scenario([[{"ok": 1}]], [[{"ok": 2}], [{"ok": 1}]])
        self.assertEqual(self.run_release()["units"][0]["result"], "unstable")

    def test_errors(self):
        self.scenario(["fail"], [[{"ok": 1}]])
        unit = self.run_release()["units"][0]
        self.assertEqual((unit["result"], unit["why"]), ("error", "official: exited with 3"))
        self.scenario(["not json"], [[{"ok": 1}]])
        self.assertEqual(self.run_release()["units"][0]["why"],
                         "official: printed something that is not JSON")

    def test_a_harness_that_cannot_be_installed(self):
        def fails(*args):
            raise sdk_canary.Stop("preparing the harness: `go get` failed")
        result = sdk_canary.run_release("go:cloud.google.com/go/firestore@1.27.0", self.official,
                                        self.hidane, self.work, self.out, self.root, fails)
        self.assertEqual((result["prepared"], result["units"]), (False, []))
        self.assertIn("go get", result["error"])


if __name__ == "__main__":
    unittest.main()
