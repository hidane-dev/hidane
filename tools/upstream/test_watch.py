"""Tests of watch.py, offline: the registries' answers are given, not fetched.

Run from the repository root:
    python3 -m unittest discover -s tools/upstream -p "test_*.py"
"""

import json
import tempfile
import unittest
import urllib.error
from pathlib import Path

import watch

PINNED_SHA = "9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c"
NEW_SHA = "a" * 64
HEAD_STABLE = "40cff80059b8dd12d2ca65f071b1c899f98e7edb"
HEAD_PREVIEW = "a050d0fed1c6f251c62c41d21af046b70d34a8bc"


def emulator_info(version, sha256, url=None, ui="1.15.0"):
    return {
        "firestore": {
            "version": version, "expectedChecksumSHA256": sha256,
            "remoteUrl": url or watch.JAR_URL.format(version=version)},
        "ui": {"snapshot": {"version": "SNAPSHOT"}, "main": {"version": ui}},
    }


class Fetch:
    """Answers GETs from a dict: URL -> object (JSON), str or bytes, or an exception."""

    def __init__(self, answers):
        self.answers = answers
        self.asked = []

    def __call__(self, url, accept="application/json"):
        self.asked.append(url)
        if url not in self.answers:
            raise urllib.error.HTTPError(url, 404, "Not Found", {}, None)
        answer = self.answers[url]
        if isinstance(answer, Exception):
            raise answer
        if isinstance(answer, bytes):
            return answer
        if isinstance(answer, str):
            return answer.encode()
        return json.dumps(answer).encode()


def registry(tools_tags=None, functions_tags=None, main_info=None, stable_info=None,
             preview_info=None):
    """The answers of every registry for the watch list in WATCH below."""
    tools_tags = tools_tags or {"latest": "15.33.0", "preview": "15.30.3-main.2"}
    raw = "https://raw.githubusercontent.com/firebase/firebase-tools/{}/src/emulator/" \
          "downloadableEmulatorInfo.json"
    answers = {
        "https://registry.npmjs.org/-/package/firebase-tools/dist-tags": tools_tags,
        "https://registry.npmjs.org/-/package/@google-cloud%2ffirestore/dist-tags":
            {"latest": "9.3.1", "next": "7.10.0", "eap-pipelines": "8.1.0-pipelines.2"},
        "https://registry.npmjs.org/-/package/firebase-functions/dist-tags":
            functions_tags or {"latest": "7.4.0", "next": "7.3.3-rc.4"},
        "https://registry.npmjs.org/firebase-tools/15.33.0": {"gitHead": HEAD_STABLE},
        "https://registry.npmjs.org/firebase-tools/15.30.3-main.2": {"gitHead": HEAD_PREVIEW},
        raw.format(HEAD_STABLE): stable_info or emulator_info("1.22.0", PINNED_SHA),
        raw.format(HEAD_PREVIEW): preview_info or emulator_info("1.22.0", PINNED_SHA),
        raw.format("main"): main_info or emulator_info("1.22.0", PINNED_SHA),
        "https://pypi.org/pypi/google-cloud-firestore/json": {
            "info": {"version": "2.34.1"},
            "releases": {
                "2.14.0rc1": [{"yanked": False}], "2.34.0": [{"yanked": False}],
                "2.34.1": [{"yanked": False}], "2.35.0": [{"yanked": True}],
                "2.36.0b1": [{"yanked": False}], "2.36.0": []}},
        "https://proxy.golang.org/cloud.google.com/go/firestore/@v/list":
            "v1.25.0\nv1.26.0\nv1.27.0-rc.1\n",
        "https://proxy.golang.org/cloud.google.com/go/firestore/@latest":
            {"Version": "v1.26.0"},
        "https://repo1.maven.org/maven2/com/google/cloud/google-cloud-firestore/"
        "maven-metadata.xml":
            "<metadata><versioning><latest>3.50.0</latest><release>3.50.0</release><versions>"
            "<version>0.25.0-beta</version><version>3.49.0</version>"
            "<version>3.50.0</version><version>3.51.0-SNAPSHOT</version>"
            "</versions></versioning></metadata>",
        "https://pub.dev/api/packages/cloud_firestore": {
            "latest": {"version": "6.10.0"},
            "versions": [{"version": "6.9.0"}, {"version": "6.10.0"},
                         {"version": "6.11.0", "retracted": True},
                         {"version": "7.0.0-dev.1"}]},
    }
    return answers


WATCH = {
    "schema_version": 1,
    "npm_prerelease_tags": ["next", "beta", "rc", "preview"],
    "packages": [
        {"ecosystem": "npm", "name": "firebase-tools", "version": "15.33.0", "verified": True},
        {"ecosystem": "npm", "name": "@google-cloud/firestore", "version": "9.3.1",
         "verified": True},
        {"ecosystem": "npm", "name": "firebase-functions", "version": "6.6.0", "verified": True},
        {"ecosystem": "pypi", "name": "google-cloud-firestore", "version": "2.34.1",
         "verified": True},
        {"ecosystem": "go", "name": "cloud.google.com/go/firestore", "version": "1.26.0",
         "verified": True},
        {"ecosystem": "maven", "name": "com.google.cloud:google-cloud-firestore",
         "version": "3.49.0", "verified": True},
        {"ecosystem": "pub", "name": "cloud_firestore", "version": "6.10.0", "verified": True},
    ],
    "emulator": {"name": "cloud-firestore-emulator", "version": "1.22.0", "sha256": PINNED_SHA,
                 "ui": "1.15.0"},
}


def package(report, ident):
    return next(p for p in report["packages"] if p["id"] == ident)


class Versions(unittest.TestCase):
    def test_prereleases(self):
        for version in ("15.30.3-main.2", "2.14.0rc1", "2.0.0.dev1", "0.25.0-beta",
                        "13.0.0-20261007162724", "7.3.3-rc.4", "1.27.0-rc.1", "7.0.0-dev.1",
                        "1.0.0-1.0.nullsafety.0"):
            self.assertTrue(watch.is_prerelease(version), version)
        for version in ("15.33.0", "2.34.1", "3.49.0", "1.0.0+build.5", "1.0.0.post1", "1.26"):
            self.assertFalse(watch.is_prerelease(version), version)

    def test_order(self):
        ordered = ["1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-beta.2", "1.0.0-beta.11",
                   "1.0.0-rc.1", "1.0.0", "1.0.0.post1", "1.0.1", "1.1", "2.0.0.dev1",
                   "2.0.0a1", "2.0.0b1", "2.0.0rc1", "2.0.0", "10.0.0"]
        self.assertEqual(sorted(reversed(ordered), key=watch.key), ordered)
        self.assertTrue(watch.newer("7.4.0", "7.3.3-rc.4"))
        self.assertFalse(watch.newer("13.0.0-20261007162724", "13.0.0"))
        self.assertTrue(watch.newer("1.0.0", None))
        self.assertFalse(watch.newer(None, "1.0.0"))

    def test_patterns(self):
        for value in ("15.33.0", "v1.26.0"[1:], "2.14.0rc1", "1.0.0+incompatible"):
            self.assertTrue(watch.valid(watch.VERSION, value), value)
        for value in ("", "latest", "1.0.0; rm -rf /", "1.0.0`x`", "1.0.0\n", "$(x)", "1" * 101,
                      "../1.0.0", None, 15):
            self.assertFalse(watch.valid(watch.VERSION, value), value)
        self.assertFalse(watch.valid(watch.EMULATOR_VERSION, "1.23.0-beta"))
        self.assertFalse(watch.valid(watch.SHA256, "A" * 64))

    def test_shown(self):
        self.assertEqual(watch.shown("1.0.0`|<b>"), "1.0.0???b?")
        self.assertEqual(len(watch.shown("x" * 500)), 64)


class Detect(unittest.TestCase):
    def test_channels(self):
        report = watch.detect(WATCH, Fetch(registry()))
        self.assertEqual(report["errors"], [])
        tools = package(report, "npm:firebase-tools")
        # The preview dist-tag is older than the stable release: no prerelease channel.
        self.assertEqual(tools["channels"], {"stable": "15.33.0"})
        self.assertEqual(tools["new"], [])
        gcf = package(report, "npm:@google-cloud/firestore")
        self.assertEqual(gcf["channels"], {"stable": "9.3.1"})  # next is final and older
        functions = package(report, "npm:firebase-functions")
        self.assertEqual(functions["new"], [{"channel": "stable", "version": "7.4.0"}])
        python = package(report, "pypi:google-cloud-firestore")
        # 2.35.0 is yanked, 2.36.0 has no files.
        self.assertEqual(python["channels"], {"stable": "2.34.1", "prerelease": "2.36.0b1"})
        self.assertEqual(python["new"], [{"channel": "prerelease", "version": "2.36.0b1"}])
        go = package(report, "go:cloud.google.com/go/firestore")
        self.assertEqual(go["channels"], {"stable": "1.26.0", "prerelease": "1.27.0-rc.1"})
        java = package(report, "maven:com.google.cloud:google-cloud-firestore")
        self.assertEqual(java["channels"], {"stable": "3.50.0"})
        self.assertEqual(java["new"], [{"channel": "stable", "version": "3.50.0"}])
        dart = package(report, "pub:cloud_firestore")
        self.assertEqual(dart["channels"], {"stable": "6.10.0", "prerelease": "7.0.0-dev.1"})
        self.assertEqual(report["emulator"]["candidates"], [])
        self.assertEqual([p["source"] for p in report["emulator"]["pins"]],
                         ["firebase-tools 15.33.0", "firebase-tools main branch"])

    def test_latest_apart_from_stable(self):
        answers = registry(tools_tags={"latest": "16.0.0-rc.1", "next": "16.0.0-rc.2"})
        answers["https://registry.npmjs.org/firebase-tools/16.0.0-rc.1"] = {"gitHead": "b" * 40}
        answers["https://registry.npmjs.org/firebase-tools/16.0.0-rc.2"] = {}  # no gitHead
        raw = watch.EMULATOR_INFO
        answers[raw.format(ref="b" * 40)] = emulator_info("1.22.0", PINNED_SHA)
        answers[raw.format(ref="v16.0.0-rc.2")] = emulator_info("1.23.0", NEW_SHA)
        report = watch.detect(WATCH, Fetch(answers))
        tools = package(report, "npm:firebase-tools")
        self.assertEqual(tools["channels"], {"latest": "16.0.0-rc.1", "prerelease": "16.0.0-rc.2"})
        self.assertEqual([n["channel"] for n in tools["new"]], ["latest", "prerelease"])
        [candidate] = report["emulator"]["candidates"]
        self.assertEqual((candidate["version"], candidate["channel"]), ("1.23.0", "prerelease"))
        self.assertEqual(candidate["pinned_by"], ["firebase-tools 16.0.0-rc.2"])
        self.assertEqual(report["errors"], [])

    def test_new_emulator(self):
        answers = registry(tools_tags={"latest": "15.34.0", "beta": "15.35.0-beta.1"},
                           main_info=emulator_info("1.23.0", NEW_SHA, ui="1.16.0"))
        answers["https://registry.npmjs.org/firebase-tools/15.34.0"] = {"gitHead": "c" * 40}
        answers["https://registry.npmjs.org/firebase-tools/15.35.0-beta.1"] = {
            "gitHead": "d" * 40}
        raw = watch.EMULATOR_INFO
        answers[raw.format(ref="c" * 40)] = emulator_info("1.23.0", NEW_SHA, ui="1.16.0")
        answers[raw.format(ref="d" * 40)] = emulator_info("1.24.0", "b" * 64)
        report = watch.detect(WATCH, Fetch(answers))
        candidates = report["emulator"]["candidates"]
        self.assertEqual([(c["version"], c["channels"]) for c in candidates],
                         [("1.23.0", ["stable", "unreleased"]), ("1.24.0", ["prerelease"])])
        self.assertEqual(candidates[0]["pinned_by"],
                         ["firebase-tools 15.34.0", "firebase-tools main branch"])
        self.assertEqual(candidates[0]["url"], watch.JAR_URL.format(version="1.23.0"))
        self.assertEqual(report["emulator"]["new_ui"], ["1.16.0"])
        emulators, sdks = watch.canary_inputs(report)
        self.assertEqual([e["key"] for e in emulators],
                         ["emulator:1.23.0:aaaaaaaaaaaa", "emulator:1.24.0:bbbbbbbbbbbb"])
        self.assertIn("npm:firebase-tools@15.34.0", [s["key"] for s in sdks])
        self.assertIn("**v1.23.0**", watch.summary(report))

    def test_unexpected_jar_url_is_not_fetched(self):
        info = emulator_info("1.23.0", NEW_SHA, url="https://example.com/emulator.jar")
        report = watch.detect(WATCH, Fetch(registry(main_info=info)))
        [candidate] = report["emulator"]["candidates"]
        self.assertFalse(candidate["url_ok"])
        self.assertEqual(watch.canary_inputs(report)[0], [])
        self.assertEqual(len(report["errors"]), 1)
        self.assertIn("another URL", report["errors"][0])

    def test_invalid_values_are_errors(self):
        answers = registry(tools_tags={"latest": "15.33.0; curl x"},
                           main_info=emulator_info("1.23.0`", NEW_SHA))
        answers["https://pub.dev/api/packages/cloud_firestore"] = {"latest": {"version": "x y"},
                                                                   "versions": []}
        report = watch.detect(WATCH, Fetch(answers))
        errors = "\n".join(report["errors"])
        self.assertIn("npm:firebase-tools: ", errors)
        self.assertIn("an invalid version (15.33.0??curl?x)", errors)
        self.assertIn("an invalid emulator version (1.23.0?)", errors)
        self.assertIn("pub:cloud_firestore", errors)
        self.assertEqual(report["emulator"]["candidates"], [])
        self.assertNotIn("curl x", watch.summary(report))

    def test_fetch_errors_are_reported(self):
        answers = registry()
        del answers["https://pypi.org/pypi/google-cloud-firestore/json"]
        answers["https://proxy.golang.org/cloud.google.com/go/firestore/@latest"] = \
            urllib.error.URLError("down")
        report = watch.detect(WATCH, Fetch(answers))
        self.assertEqual(package(report, "pypi:google-cloud-firestore")["error"], "HTTP 404")
        self.assertEqual(package(report, "go:cloud.google.com/go/firestore")["error"],
                         "URLError")
        self.assertEqual(len(report["errors"]), 2)
        self.assertIn("### Errors", watch.summary(report))

    def test_outputs(self):
        report = watch.detect(WATCH, Fetch(registry()))
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / "out"
            watch.write_outputs(out, report)
            lines = dict(line.split("=", 1) for line in out.read_text().splitlines())
        self.assertEqual(json.loads(lines["emulators"]), [])
        sdks = json.loads(lines["sdks"])
        self.assertEqual(sorted(s["key"] for s in sdks), [
            "go:cloud.google.com/go/firestore@1.27.0-rc.1",
            "maven:com.google.cloud:google-cloud-firestore@3.50.0",
            "npm:firebase-functions@7.4.0",
            "pub:cloud_firestore@7.0.0-dev.1",
            "pypi:google-cloud-firestore@2.36.0b1"])
        self.assertRegex(lines["fingerprint"], r"^[0-9a-f]{64}$")
        self.assertEqual(lines["new"], "5")
        self.assertEqual(lines["errors"], "0")
        self.assertEqual(lines["fingerprint"], watch.fingerprint(*watch.canary_inputs(report)))

    def test_requested_emulator(self):
        report = watch.detect(WATCH, Fetch(registry()), [("1.22.0", PINNED_SHA)])
        [candidate] = report["emulator"]["candidates"]
        self.assertEqual((candidate["channel"], candidate["slug"], candidate["pinned_by"]),
                         ("requested", "v1.22.0-9b6498b7f627", [watch.REQUEST_SOURCE]))
        self.assertEqual(watch.origin(candidate["pinned_by"]), "requested by hand")
        self.assertEqual(watch.origin(["firebase-tools 1.0.0", watch.REQUEST_SOURCE]),
                         "pinned by firebase-tools 1.0.0; requested by hand")
        [emulator], _ = watch.canary_inputs(report)
        self.assertEqual(emulator["url"], watch.JAR_URL.format(version="1.22.0"))
        for bad in (("1.22.0; x", PINNED_SHA), ("1.22.0", "B" * 64), ("", "")):
            with self.assertRaises(watch.Stop):
                watch.detect(WATCH, Fetch(registry()), [bad])

    def test_requested_and_pinned_by_a_release(self):
        answers = registry(main_info=emulator_info("1.23.0", NEW_SHA))
        report = watch.detect(WATCH, Fetch(answers), [("1.23.0", NEW_SHA)])
        [candidate] = report["emulator"]["candidates"]
        self.assertEqual(candidate["channels"], ["unreleased", "requested"])

    def test_requested_sdks(self):
        report = watch.detect(WATCH, Fetch(registry()), (), [
            "go:cloud.google.com/go/firestore", "npm:@google-cloud/firestore@9.4.0",
            "go:cloud.google.com/go/firestore@1.27.0-rc.1"])
        # A new release asked for keeps its channel; another release is `requested`.
        self.assertEqual(package(report, "go:cloud.google.com/go/firestore")["new"], [
            {"channel": "prerelease", "version": "1.27.0-rc.1", "requested": True},
            {"channel": "requested", "version": "1.26.0", "requested": True}])
        self.assertEqual(package(report, "npm:@google-cloud/firestore")["new"],
                         [{"channel": "requested", "version": "9.4.0", "requested": True}])
        _, sdks = watch.canary_inputs(report)
        self.assertEqual([(s["version"], s["requested"]) for s in sdks
                          if s["ecosystem"] == "pypi"], [("2.36.0b1", False)])
        for bad in ("npm:left-pad@1.0.0", "npm:@google-cloud/firestore@x y", "go:", ""):
            with self.assertRaises(watch.Stop):
                watch.detect(WATCH, Fetch(registry()), (), [bad])

    def test_go_module_escape(self):
        self.assertEqual(watch.go_escape("github.com/Azure/azure-sdk"),
                         "github.com/!azure/azure-sdk")


VERIFIED_RS = '''
pub const VERIFIED: &[Verified] = &[
    Verified {
        ecosystem: "npm",
        name: "firebase-tools",
        version: "15.33.0",
        what: "emulators:start",
        source: "docs",
    },
    Verified {
        ecosystem: "go",
        name: "cloud.google.com/go/firestore",
        version: "1.26.0",
        what: "documents",
        source: "docs",
    },
];
pub const EMULATOR_UI_VERSION: &str = "v1.15.0, the version firebase-tools 15.33.0 downloads";
'''
PINNED_SH = f"# comment\nPINNED_VERSION=1.22.0\nPINNED_SHA256={PINNED_SHA}\n"


class Check(unittest.TestCase):
    def watch_list(self, **changes):
        data = {
            "schema_version": 1,
            "packages": [
                {"ecosystem": "npm", "name": "firebase-tools", "version": "15.33.0",
                 "verified": True},
                {"ecosystem": "go", "name": "cloud.google.com/go/firestore", "version": "1.26.0",
                 "verified": True},
                {"ecosystem": "npm", "name": "firebase-admin", "version": "14.5.0",
                 "verified": False},
            ],
            "emulator": {"name": "cloud-firestore-emulator", "version": "1.22.0",
                         "sha256": PINNED_SHA, "ui": "1.15.0"},
        }
        data.update(changes)
        return data

    def test_in_step(self):
        self.assertEqual(watch.check(self.watch_list(), VERIFIED_RS, PINNED_SH), [])

    def test_out_of_step(self):
        data = self.watch_list()
        data["packages"][1]["version"] = "1.27.0"
        data["packages"][0]["verified"] = False
        data["packages"][2]["verified"] = True
        data["emulator"] = dict(data["emulator"], version="1.23.0", ui="1.16.0")
        problems = "\n".join(watch.check(data, VERIFIED_RS, PINNED_SH))
        self.assertIn("npm:firebase-tools is verified", problems)
        self.assertIn("go:cloud.google.com/go/firestore: tools/upstream/watch.json says 1.27.0",
                      problems)
        self.assertIn("npm:firebase-admin: verified in", problems)
        self.assertIn("emulator: tools/upstream/watch.json says 1.23.0", problems)
        self.assertIn("emulator ui", problems)

    def test_certification_and_compatibility(self):
        certification = {"verified_packages": [
            {"ecosystem": "npm", "name": "firebase-tools", "version": "15.33.0"},
            {"ecosystem": "go", "name": "cloud.google.com/go/firestore", "version": "1.27.0"}]}
        compatibility = {"official_emulator": {"product": "cloud-firestore-emulator",
                                               "release": "1.22.0", "jar_sha256": PINNED_SHA}}
        # sdk-certification.json wins over verified.rs.
        problems = watch.check(self.watch_list(), VERIFIED_RS, PINNED_SH, certification,
                               compatibility)
        self.assertEqual(problems, ["go:cloud.google.com/go/firestore: tools/upstream/watch.json "
                                    "says 1.26.0, docs/sdk-certification.json says 1.27.0"])
        compatibility["official_emulator"]["release"] = "1.23.0"
        problems = watch.check(self.watch_list(), VERIFIED_RS, PINNED_SH, None, compatibility)
        self.assertEqual(len(problems), 1)
        self.assertIn("docs/compatibility.json", problems[0])

    def test_missing_from_watch_list(self):
        data = self.watch_list()
        del data["packages"][1]
        problems = watch.check(data, VERIFIED_RS, PINNED_SH)
        self.assertEqual(len(problems), 1)
        self.assertIn("not watched", problems[0])

    def test_load_rejects_bad_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "watch.json"
            data = self.watch_list()
            data["packages"].append({"ecosystem": "npm", "name": "Bad Name", "version": "x",
                                     "verified": "yes"})
            data["packages"].append(dict(data["packages"][0]))
            path.write_text(json.dumps(data))
            with self.assertRaises(watch.Stop) as caught:
                watch.load_watch(path)
        message = str(caught.exception)
        for phrase in ("not a valid npm name", "version is not a version",
                       "verified is not true or false", "listed twice"):
            self.assertIn(phrase, message)

    def test_repository(self):
        """The committed watch list is in step with verified.rs and pinned.sh."""
        data = watch.load_watch()
        problems = watch.check(data, (watch.ROOT / watch.VERIFIED_RS).read_text(),
                               (watch.ROOT / watch.PINNED_SH).read_text(),
                               watch.optional_json(watch.SDK_CERTIFICATION),
                               watch.optional_json(watch.COMPATIBILITY))
        self.assertEqual(problems, [])
        entries, ui = watch.verified_rs((watch.ROOT / watch.VERIFIED_RS).read_text())
        self.assertGreaterEqual(len(entries), 9)
        self.assertEqual(ui, data["emulator"]["ui"])


if __name__ == "__main__":
    unittest.main()
