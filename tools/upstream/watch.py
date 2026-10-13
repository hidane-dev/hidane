"""Watches the upstream releases hidane is verified against (tools/upstream/README.md).

Usage (from the repository root):
    python3 tools/upstream/watch.py check
        Checks tools/upstream/watch.json against what the repository says is verified:
        docs/sdk-certification.json (or, without it, crates/hidane/src/project/verified.rs),
        .github/actions/official-emulator/pinned.sh and docs/compatibility.json. Offline.
    python3 tools/upstream/watch.py detect [--report FILE] [--summary FILE] [--outputs FILE]
            [--emulator VERSION SHA256]...
        Reads the public metadata of each watched package's registry and of firebase-tools, and
        writes what it found: the report (JSON), a Markdown summary, and the inputs of the
        compatibility canary as GitHub Actions outputs (`name=value` lines). `--emulator` also
        hands the canary an official emulator jar by hand (the pinned one too), as `requested`.

Only public metadata is read, never a page meant for people: npm's dist-tags and version
documents, PyPI's JSON API, the Go module proxy, Maven Central's maven-metadata.xml, pub.dev's
API, and the file firebase-tools pins its emulators in (src/emulator/downloadableEmulatorInfo.json)
at the commit each of its releases was published from (`gitHead` in npm's version document), and
on its default branch. Every string taken from them is checked against a strict pattern before it
is used or written anywhere; one that does not match is reported as an error, not used.

Each package is seen on three channels:
- stable: the newest final release (no prerelease part);
- latest: the release the registry hands out by default (npm's `latest` dist-tag, PyPI's
  `info.version`, Go's `@latest`, Maven's `<latest>`, pub.dev's `latest`); reported apart from
  stable only when it differs;
- prerelease: the newest prerelease (rc, beta, alpha, dev, next, preview) newer than stable; on
  npm, from the dist-tags in `npm_prerelease_tags`.
A release on any channel newer than the watched `version` is new. The official emulator is new
when a firebase-tools release on any channel, or its default branch, pins another jar than
`emulator` (version or SHA-256).

Stdlib only.
"""

import argparse
import hashlib
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WATCH = "tools/upstream/watch.json"
VERIFIED_RS = "crates/hidane/src/project/verified.rs"
PINNED_SH = ".github/actions/official-emulator/pinned.sh"
# Generated from the recorded results by tools/parity/; read when they are there.
SDK_CERTIFICATION = "docs/sdk-certification.json"
COMPATIBILITY = "docs/compatibility.json"
SCHEMA_VERSION = 1

ECOSYSTEMS = ("npm", "pypi", "go", "maven", "pub")
STABLE, LATEST, PRERELEASE, UNRELEASED = "stable", "latest", "prerelease", "unreleased"
REQUESTED = "requested"  # a jar the canary was asked to run against by hand
REQUEST_SOURCE = "a request by hand"
CHANNELS = (STABLE, LATEST, PRERELEASE)

# Strict patterns for every string read from a registry.
VERSION = re.compile(r"[0-9]{1,9}(?:\.[0-9]{1,9}){0,3}(?:[-+.]?[0-9A-Za-z]{1,40}){0,8}")
SHA256 = re.compile(r"[0-9a-f]{64}")
GIT_COMMIT = re.compile(r"[0-9a-f]{40}")
# The official emulator's versions, as .github/actions/official-emulator accepts them.
EMULATOR_VERSION = re.compile(r"[0-9]{1,6}(?:\.[0-9]{1,6}){1,3}")
DIST_TAG = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")
NPM_NAME = re.compile(r"(?:@[a-z0-9][a-z0-9._-]{0,63}/)?[a-z0-9][a-z0-9._-]{0,99}")
PYPI_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,99}")
GO_MODULE = re.compile(r"[a-z0-9.-]{1,100}(?:/[A-Za-z0-9._~-]{1,100}){1,8}")
MAVEN_NAME = re.compile(r"[A-Za-z0-9_.-]{1,100}:[A-Za-z0-9_.-]{1,100}")
PUB_NAME = re.compile(r"[a-z][a-z0-9_]{0,63}")
NAMES = {"npm": NPM_NAME, "pypi": PYPI_NAME, "go": GO_MODULE, "maven": MAVEN_NAME, "pub": PUB_NAME}

FIREBASE_TOOLS = "firebase-tools"
FIREBASE_TOOLS_DEFAULT_BRANCH = "main"
EMULATOR_INFO = ("https://raw.githubusercontent.com/firebase/firebase-tools/{ref}/"
                 "src/emulator/downloadableEmulatorInfo.json")
# The only place a jar is fetched from: where firebase-tools downloads it.
JAR_URL = ("https://storage.googleapis.com/firebase-preview-drop/emulator/"
           "cloud-firestore-emulator-v{version}.jar")
USER_AGENT = "hidane-upstream-watcher (+https://github.com/hidane-dev/hidane)"


class Stop(Exception):
    pass


def valid(pattern, value):
    return isinstance(value, str) and len(value) <= 100 and pattern.fullmatch(value) is not None


def shown(value):
    """A value read from outside, safe to print in Markdown or a log line."""
    text = value if isinstance(value, str) else repr(value)
    return re.sub(r"[^0-9A-Za-z._:@/+-]", "?", text)[:64]


# Versions -------------------------------------------------------------------------------------

_SPLIT = re.compile(r"^v?([0-9]+(?:\.[0-9]+)*)(.*)$")
_POST = re.compile(r"^[.-]?(?:post|sp\.?)[0-9]*$", re.I)
# PEP 440's prerelease kinds, in their order; other prerelease words sort after `rc`.
_PEP440 = {"dev": 0, "a": 1, "alpha": 1, "b": 2, "beta": 2, "c": 3, "rc": 3, "pre": 3,
           "preview": 3}


def parse(version):
    """(release numbers, prerelease part or None, post-release part) of a version string.

    Handles SemVer (npm, Go, pub), PEP 440 (PyPI) and Maven's qualifiers well enough to tell a
    prerelease from a final release and to order the releases of one package."""
    match = _SPLIT.match(version)
    if not match:
        raise ValueError(f"not a version: {shown(version)}")
    release = tuple(int(part) for part in match.group(1).split("."))
    rest = match.group(2).split("+", 1)[0]  # SemVer build metadata does not order
    if not rest:
        return release, None, ""
    if _POST.match(rest):
        return release, None, rest
    return release, rest.lstrip(".-"), ""


def is_prerelease(version):
    return parse(version)[1] is not None


def _identifiers(text):
    out = []
    for token in re.findall(r"[0-9]+|[A-Za-z]+", text):
        if token.isdigit():
            out.append((0, int(token), ""))
        else:
            lower = token.lower()
            out.append((1, _PEP440.get(lower, 4), lower))
    return tuple(out)


def key(version):
    """A sort key: releases by number, a prerelease before its final release, and a post
    release after it."""
    release, pre, post = parse(version)
    padded = release + (0,) * (6 - len(release))
    if pre is None:
        return padded, 1, (), _identifiers(post)
    return padded, 0, _identifiers(pre), ()


def newer(a, b):
    """Whether version a is newer than version b; any version is newer than None."""
    if a is None:
        return False
    if b is None:
        return True
    return key(a) > key(b)


def newest(versions):
    versions = list(versions)
    return max(versions, key=key) if versions else None


# Fetching -------------------------------------------------------------------------------------

def http_get(url, accept="application/json"):
    """GET a URL; bytes, or raises after three attempts."""
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Accept": accept})
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                return response.read(20_000_000)
        except urllib.error.HTTPError as err:
            if err.code < 500 and err.code != 429:
                raise
            last = err
        except (urllib.error.URLError, TimeoutError) as err:
            last = err
        time.sleep(2 ** attempt)
    raise last


def fetch_json(fetch, url, accept="application/json"):
    try:
        return json.loads(fetch(url, accept))
    except (ValueError, UnicodeDecodeError) as err:
        raise Stop(f"{url}: not JSON ({type(err).__name__})")


def checked_version(value, where):
    if not valid(VERSION, value):
        raise Stop(f"{where}: an invalid version ({shown(value)})")
    return value


def channels(stable, latest, prerelease, sources):
    """The channels of a package, with where each came from. `latest` is kept only when it is
    not the stable release, and a prerelease only when it is newer than the stable one."""
    if prerelease is not None and not newer(prerelease, stable or latest):
        prerelease = None
    seen = {STABLE: stable, LATEST: latest if latest != stable else None, PRERELEASE: prerelease}
    return ({channel: version for channel, version in seen.items() if version is not None},
            {channel: sources[channel] for channel, version in seen.items() if version is not None})


def from_list(versions, latest, where, latest_source):
    """The channels of a registry that lists every version, and names its default one."""
    # A historical version in a form this script does not read is left out, not an error.
    checked = [v for v in versions if valid(VERSION, v)]
    final = [v for v in checked if not is_prerelease(v)]
    pre = [v for v in checked if is_prerelease(v)]
    if latest is not None:
        checked_version(latest, where)
    return channels(newest(final), latest, newest(pre),
                    {STABLE: "newest final release", LATEST: latest_source,
                     PRERELEASE: "newest prerelease"})


def npm(name, fetch, prerelease_tags):
    url = f"https://registry.npmjs.org/-/package/{name.replace('/', '%2f')}/dist-tags"
    tags = fetch_json(fetch, url)
    if not isinstance(tags, dict) or "latest" not in tags:
        raise Stop(f"{url}: no `latest` dist-tag")
    latest = checked_version(tags["latest"], url)
    pre = {}
    for tag in prerelease_tags:
        version = tags.get(tag)
        if version is None:
            continue
        checked_version(version, f"{url} ({tag})")
        if is_prerelease(version):
            pre[version] = tag
    prerelease = newest(pre)
    stable = None if is_prerelease(latest) else latest
    return channels(stable, latest, prerelease,
                    {STABLE: "dist-tag latest", LATEST: "dist-tag latest",
                     PRERELEASE: f"dist-tag {pre.get(prerelease, '')}"})


def pypi(name, fetch):
    url = f"https://pypi.org/pypi/{name}/json"
    data = fetch_json(fetch, url)
    try:
        latest = data["info"]["version"]
        releases = data["releases"]
    except (KeyError, TypeError):
        raise Stop(f"{url}: no `info.version` or `releases`")
    # A release whose every file is yanked is gone; one without files is not installable.
    versions = [v for v, files in releases.items()
                if isinstance(files, list) and files
                and not all(isinstance(f, dict) and f.get("yanked") for f in files)]
    return from_list(versions, latest, url, "info.version")


def go_escape(module):
    """The module path as the proxy wants it: an upper-case letter becomes `!` and its lower case."""
    return re.sub(r"[A-Z]", lambda m: "!" + m.group(0).lower(), module)


def go(module, fetch):
    base = f"https://proxy.golang.org/{go_escape(module)}/@v"
    listed = fetch(f"{base}/list", "text/plain").decode("utf-8").split()
    latest = fetch_json(fetch, f"https://proxy.golang.org/{go_escape(module)}/@latest")
    if not isinstance(latest, dict) or not isinstance(latest.get("Version"), str):
        raise Stop(f"{base}: @latest has no Version")
    strip = lambda v: v[1:] if v.startswith("v") else v  # noqa: E731
    return from_list([strip(v) for v in listed], strip(latest["Version"]), f"{base}/list",
                     "@latest")


def maven(coordinate, fetch):
    group, artifact = coordinate.split(":")
    url = (f"https://repo1.maven.org/maven2/{group.replace('.', '/')}/{artifact}/"
           "maven-metadata.xml")
    text = fetch(url, "application/xml").decode("utf-8")
    # A flat document: read the few elements needed with patterns rather than an XML parser.
    versions = re.findall(r"<version>\s*([^<\s]+)\s*</version>", text)
    latest = re.search(r"<latest>\s*([^<\s]+)\s*</latest>", text)
    versions = [v for v in versions if not v.upper().endswith("-SNAPSHOT")]
    if not versions:
        raise Stop(f"{url}: no versions")
    return from_list(versions, latest.group(1) if latest else None, url, "<latest>")


def pub(name, fetch):
    url = f"https://pub.dev/api/packages/{name}"
    data = fetch_json(fetch, url, "application/vnd.pub.v2+json")
    try:
        latest = data["latest"]["version"]
        versions = [v["version"] for v in data["versions"] if not v.get("retracted")]
    except (KeyError, TypeError):
        raise Stop(f"{url}: no `latest` or `versions`")
    return from_list(versions, latest, url, "latest")


def observe(package, fetch, prerelease_tags):
    ecosystem, name = package["ecosystem"], package["name"]
    if ecosystem == "npm":
        return npm(name, fetch, prerelease_tags)
    if ecosystem == "pypi":
        return pypi(name, fetch)
    if ecosystem == "go":
        return go(name, fetch)
    if ecosystem == "maven":
        return maven(name, fetch)
    if ecosystem == "pub":
        return pub(name, fetch)
    raise Stop(f"{ecosystem}: not a watched ecosystem")


# The official emulator --------------------------------------------------------------------------

def emulator_pin(info, where):
    """The Firestore emulator (and the Emulator UI version) a downloadableEmulatorInfo.json pins."""
    try:
        firestore = info["firestore"]
        version, sha256, url = (firestore["version"], firestore["expectedChecksumSHA256"],
                                firestore["remoteUrl"])
    except (KeyError, TypeError):
        raise Stop(f"{where}: no firestore version, expectedChecksumSHA256 or remoteUrl")
    if not valid(EMULATOR_VERSION, version):
        raise Stop(f"{where}: an invalid emulator version ({shown(version)})")
    if not valid(SHA256, sha256):
        raise Stop(f"{where}: an invalid SHA-256 ({shown(sha256)})")
    ui = info.get("ui") if isinstance(info, dict) else None
    if isinstance(ui, dict) and isinstance(ui.get("main"), dict):
        ui = ui["main"]
    ui_version = ui.get("version") if isinstance(ui, dict) else None
    if ui_version is not None and not valid(VERSION, ui_version):
        ui_version = None
    pin = {"version": version, "sha256": sha256, "ui": ui_version,
           "url": JAR_URL.format(version=version)}
    # The jar is only ever fetched from where firebase-tools has always downloaded it.
    pin["url_ok"] = url == pin["url"]
    if not pin["url_ok"]:
        pin["published_url"] = shown(url)
    return pin


def firebase_tools_ref(version, fetch):
    """The commit firebase-tools `version` was published from (npm's gitHead), or its tag."""
    url = f"https://registry.npmjs.org/{FIREBASE_TOOLS}/{version}"
    head = fetch_json(fetch, url).get("gitHead")
    return head if valid(GIT_COMMIT, head) else f"v{version}"


def emulator_pins(tools_channels, fetch):
    """What each firebase-tools channel, and its default branch, pins; errors apart."""
    by_version = {}
    for channel, version in tools_channels.items():
        by_version.setdefault(version, []).append(channel)
    pins, errors = [], []
    targets = [(f"{FIREBASE_TOOLS} {version}", version, chans)
               for version, chans in by_version.items()]
    targets.append((f"{FIREBASE_TOOLS} {FIREBASE_TOOLS_DEFAULT_BRANCH} branch", None, [UNRELEASED]))
    for label, version, chans in targets:
        try:
            ref = (firebase_tools_ref(version, fetch) if version is not None
                   else FIREBASE_TOOLS_DEFAULT_BRANCH)
            url = EMULATOR_INFO.format(ref=ref)
            pin = emulator_pin(fetch_json(fetch, url), url)
        except (Stop, OSError, ValueError) as err:
            errors.append(f"{label}: {shown_error(err)}")
            continue
        pin.update(source=label, ref=ref, channels=chans)
        pins.append(pin)
    return pins, errors


def emulator_candidates(pins, pinned, requested=()):
    """The jars the pins name that are not the pinned one, each once, with who pins it, and the
    ones requested by hand (any jar, the pinned one too)."""
    found = {}
    for version, sha256 in requested:
        if not valid(EMULATOR_VERSION, version) or not valid(SHA256, sha256):
            raise Stop("a requested emulator needs a version (1.22.0) and its jar's SHA-256 (64 "
                       "lower-case hexadecimal digits)")
        pins = [*pins, {"version": version, "sha256": sha256, "url_ok": True,
                        "url": JAR_URL.format(version=version), "source": REQUEST_SOURCE,
                        "channels": [REQUESTED]}]
    for pin in pins:
        if ((pin["version"], pin["sha256"]) == (pinned["version"], pinned["sha256"])
                and REQUESTED not in pin["channels"]):
            continue
        entry = found.setdefault((pin["version"], pin["sha256"]), {
            "version": pin["version"], "sha256": pin["sha256"], "url": pin["url"],
            "url_ok": pin["url_ok"], "channels": [], "pinned_by": []})
        entry["url_ok"] = entry["url_ok"] and pin["url_ok"]
        entry["pinned_by"].append(pin["source"])
        for channel in pin["channels"]:
            if channel not in entry["channels"]:
                entry["channels"].append(channel)
    order = [STABLE, LATEST, PRERELEASE, UNRELEASED, REQUESTED]
    out = []
    for entry in found.values():
        entry["channels"].sort(key=order.index)
        entry["channel"] = entry["channels"][0]
        entry["key"] = f"emulator:{entry['version']}:{entry['sha256'][:12]}"
        # Names its artifacts in the canary.
        entry["slug"] = f"v{entry['version']}-{entry['sha256'][:12]}"
        out.append(entry)
    return sorted(out, key=lambda e: key(e["version"]))


def origin(pinned_by):
    """Who names a jar, as a phrase: `pinned by firebase-tools 15.34.0; requested by hand`."""
    pins = [p for p in pinned_by if p != REQUEST_SOURCE]
    parts = [f"pinned by {', '.join(pins)}"] if pins else []
    if REQUEST_SOURCE in pinned_by:
        parts.append("requested by hand")
    return "; ".join(parts) or "named by nothing"


def shown_error(err):
    if isinstance(err, urllib.error.HTTPError):
        return f"HTTP {err.code}"
    if isinstance(err, Stop):
        return str(err)
    return type(err).__name__


# The watch list ---------------------------------------------------------------------------------

def load_watch(path=None):
    path = Path(path) if path else ROOT / WATCH
    with open(path, encoding="utf-8") as f:
        watch = json.load(f)
    problems = []
    if watch.get("schema_version") != SCHEMA_VERSION:
        problems.append(f"schema_version is not {SCHEMA_VERSION}")
    seen = set()
    for package in watch.get("packages", []):
        ecosystem, name = package.get("ecosystem"), package.get("name")
        where = f"{ecosystem}:{name}"
        if ecosystem not in ECOSYSTEMS:
            problems.append(f"{where}: ecosystem is not one of {', '.join(ECOSYSTEMS)}")
            continue
        if not valid(NAMES[ecosystem], name):
            problems.append(f"{where}: not a valid {ecosystem} name")
        if not valid(VERSION, package.get("version")):
            problems.append(f"{where}: version is not a version")
        if not isinstance(package.get("verified"), bool):
            problems.append(f"{where}: verified is not true or false")
        if where in seen:
            problems.append(f"{where}: listed twice")
        seen.add(where)
    for tag in watch.get("npm_prerelease_tags", []):
        if not valid(DIST_TAG, tag) or tag == "latest":
            problems.append(f"npm_prerelease_tags: {tag!r} is not a prerelease dist-tag")
    emulator = watch.get("emulator", {})
    if not valid(VERSION, emulator.get("version")) or not valid(SHA256, emulator.get("sha256")):
        problems.append("emulator: needs a version and a SHA-256")
    if not valid(VERSION, emulator.get("ui")):
        problems.append("emulator: ui is not a version")
    if problems:
        raise Stop(f"{WATCH}: " + "; ".join(problems))
    return watch


def verified_rs(text):
    """{(ecosystem, name): version} of the VERIFIED entries, and the Emulator UI version."""
    entries = {}
    for body in re.findall(r"Verified\s*\{(.*?)\n\s*\}", text, re.S):
        fields = dict(re.findall(r"\b(ecosystem|name|version):\s*\"([^\"]*)\"", body))
        if {"ecosystem", "name", "version"} <= fields.keys():
            entries[(fields["ecosystem"], fields["name"])] = fields["version"]
    ui = re.search(r"EMULATOR_UI_VERSION:\s*&str\s*=\s*\"v([0-9.]+)", text)
    return entries, ui.group(1) if ui else None


def pinned_sh(text):
    version = re.search(r"^PINNED_VERSION=([0-9.]+)\s*$", text, re.M)
    sha256 = re.search(r"^PINNED_SHA256=([0-9a-f]{64})\s*$", text, re.M)
    return (version.group(1) if version else None), (sha256.group(1) if sha256 else None)


def certified(data):
    """{(ecosystem, name): version} of docs/sdk-certification.json's `verified_packages`."""
    entries = {}
    for package in data.get("verified_packages", []) if isinstance(data, dict) else []:
        if isinstance(package, dict) and {"ecosystem", "name", "version"} <= package.keys():
            entries[(package["ecosystem"], package["name"])] = package["version"]
    return entries


def check(watch, verified_text, pinned_text, certification=None, compatibility=None):
    """What is out of step between the watch list and what the repository says is verified:
    docs/sdk-certification.json's `verified_packages` when there is one (verified.rs is
    generated from it), verified.rs otherwise; pinned.sh, and docs/compatibility.json's
    `official_emulator` when there is one; the Emulator UI in verified.rs."""
    problems = []
    entries, ui = verified_rs(verified_text)
    source = VERIFIED_RS
    if certification is not None:
        entries, source = certified(certification), SDK_CERTIFICATION
    if not entries:
        problems.append(f"{source}: no verified packages found; update check()")
    listed = {}
    for package in watch["packages"]:
        listed[(package["ecosystem"], package["name"])] = package
    for (ecosystem, name), version in sorted(entries.items()):
        package = listed.get((ecosystem, name))
        if package is None:
            problems.append(f"{ecosystem}:{name} is verified ({source}) but not watched; "
                            f"add it to {WATCH}")
        elif not package["verified"]:
            problems.append(f"{ecosystem}:{name} is verified ({source}); set verified: true")
        elif package["version"] != version:
            problems.append(f"{ecosystem}:{name}: {WATCH} says {package['version']}, "
                            f"{source} says {version}")
    for (ecosystem, name), package in sorted(listed.items()):
        if package["verified"] and (ecosystem, name) not in entries:
            problems.append(f"{ecosystem}:{name}: verified in {WATCH} but not in {source}")
    emulator = watch["emulator"]
    version, sha256 = pinned_sh(pinned_text)
    if (emulator["version"], emulator["sha256"]) != (version, sha256):
        problems.append(f"emulator: {WATCH} says {emulator['version']} "
                        f"({emulator['sha256'][:12]}…), {PINNED_SH} says {version} "
                        f"({(sha256 or '')[:12]}…)")
    if compatibility is not None:
        official = compatibility.get("official_emulator") if isinstance(compatibility, dict) \
            else None
        if not isinstance(official, dict) or (official.get("release"),
                                              official.get("jar_sha256")) != (
                emulator["version"], emulator["sha256"]):
            problems.append(f"emulator: {WATCH} says {emulator['version']}, {COMPATIBILITY} "
                            "(official_emulator) says otherwise")
    if emulator["ui"] != ui:
        problems.append(f"emulator ui: {WATCH} says {emulator['ui']}, {VERIFIED_RS} "
                        f"(EMULATOR_UI_VERSION) says {ui}")
    return problems


def optional_json(rel):
    path = ROOT / rel
    if not path.is_file():
        return None
    with open(path, encoding="utf-8") as f:
        return json.load(f)


# Detection --------------------------------------------------------------------------------------

def detect(watch, fetch, requested=()):
    """The report: every package's channels and new releases, and the official emulator's."""
    tags = watch.get("npm_prerelease_tags", [])
    packages, errors = [], []
    tools_channels = None
    for package in watch["packages"]:
        entry = {"ecosystem": package["ecosystem"], "name": package["name"],
                 "id": f"{package['ecosystem']}:{package['name']}",
                 "watched": package["version"], "verified": package["verified"],
                 "channels": {}, "sources": {}, "new": [], "error": None}
        try:
            entry["channels"], entry["sources"] = observe(package, fetch, tags)
        except (Stop, OSError, ValueError) as err:
            entry["error"] = shown_error(err)
            errors.append(f"{entry['id']}: {entry['error']}")
        for channel in CHANNELS:
            version = entry["channels"].get(channel)
            if version is not None and newer(version, package["version"]):
                entry["new"].append({"channel": channel, "version": version})
        if package["ecosystem"] == "npm" and package["name"] == FIREBASE_TOOLS:
            tools_channels = entry["channels"]
        packages.append(entry)
    pinned = dict(watch["emulator"])
    pins, pin_errors = emulator_pins(tools_channels or {}, fetch)
    errors += pin_errors
    candidates = emulator_candidates(pins, pinned, requested)
    ui = sorted({pin["ui"] for pin in pins if pin["ui"] and pin["ui"] != pinned["ui"]}, key=key)
    for candidate in candidates:
        if not candidate["url_ok"]:
            errors.append(f"emulator {candidate['version']}: firebase-tools downloads it from "
                          "another URL than " + JAR_URL.format(version=candidate["version"]))
    return {
        "schema_version": SCHEMA_VERSION,
        "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "packages": packages,
        "emulator": {"watched": pinned, "pins": pins, "candidates": candidates,
                     "new_ui": ui},
        "errors": errors,
    }


def canary_inputs(report):
    """What the compatibility canary runs: the new jars it can fetch, and the new packages."""
    emulators = [{k: c[k] for k in ("key", "slug", "version", "sha256", "url", "channel",
                                     "pinned_by")}
                 for c in report["emulator"]["candidates"] if c["url_ok"]]
    sdks = []
    for package in report["packages"]:
        for new in package["new"]:
            sdks.append({"key": f"{package['id']}@{new['version']}",
                         "ecosystem": package["ecosystem"], "name": package["name"],
                         "version": new["version"], "channel": new["channel"],
                         "verified": package["verified"]})
    return emulators, sdks


def fingerprint(emulators, sdks):
    """Names the set of new releases; the canary runs once for each set."""
    data = json.dumps({"emulators": emulators, "sdks": sdks}, sort_keys=True,
                      separators=(",", ":"))
    return hashlib.sha256(data.encode()).hexdigest()


def cell(value):
    return "–" if value is None else f"`{shown(value)}`"


def summary(report):
    lines = ["## Upstream releases", ""]
    new_count = sum(len(p["new"]) for p in report["packages"])
    candidates = report["emulator"]["candidates"]
    if not new_count and not candidates:
        lines += ["Nothing new: every watched package and the official emulator are where "
                  f"`{WATCH}` says.", ""]
    lines += ["| Package | Watched | Stable | Latest | Prerelease | New |",
              "|---|---|---|---|---|---|"]
    for p in report["packages"]:
        watched = f"`{p['watched']}`" + (" (verified)" if p["verified"] else " (reviewed)")
        if p["error"]:
            new = f"error: {shown(p['error'])}"
        elif p["new"]:
            new = "**" + ", ".join(f"{n['version']} ({n['channel']})" for n in p["new"]) + "**"
        else:
            new = "–"
        latest = p["channels"].get(LATEST)
        latest = ("same" if latest is None and p["channels"].get(STABLE) else cell(latest))
        lines.append(f"| {p['ecosystem']}: `{p['name']}` | {watched} | "
                     f"{cell(p['channels'].get(STABLE))} | {latest} | "
                     f"{cell(p['channels'].get(PRERELEASE))} | {new} |")
    emulator = report["emulator"]
    watched = emulator["watched"]
    lines += ["", "### Official Firestore emulator", "",
              f"Watched: `{watched['name']}` v{watched['version']} (SHA-256 "
              f"`{watched['sha256']}`), Emulator UI v{watched['ui']}.", "",
              "| Pinned by | Channel | Emulator | SHA-256 | Emulator UI |", "|---|---|---|---|---|"]
    for pin in emulator["pins"]:
        same = (pin["version"], pin["sha256"]) == (watched["version"], watched["sha256"])
        version = f"v{pin['version']}" if same else f"**v{pin['version']}**"
        lines.append(f"| {pin['source']} | {', '.join(pin['channels'])} | {version} | "
                     f"`{pin['sha256'][:12]}…` | {cell(pin['ui'])} |")
    if candidates:
        lines += ["", "New official emulators:", ""]
        for c in candidates:
            fetchable = ("the canary can fetch it" if c["url_ok"]
                         else "**not fetched**: firebase-tools names another URL")
            lines.append(f"- v{c['version']} (SHA-256 `{c['sha256']}`), {c['channel']}, "
                         f"{origin(c['pinned_by'])}; {fetchable}.")
    if emulator["new_ui"]:
        lines += ["", "Another Emulator UI is pinned: "
                  + ", ".join(f"v{v}" for v in emulator["new_ui"])
                  + f" (verified: v{watched['ui']}). The canary does not run the Emulator UI."]
    if report["errors"]:
        lines += ["", "### Errors", ""] + [f"- {shown_line(e)}" for e in report["errors"]]
    lines += ["", "What to do with a new release: `tools/upstream/README.md`.", ""]
    return "\n".join(lines)


def shown_line(text):
    """An error line (built from validated or shown() values), without Markdown controls."""
    return re.sub(r"[`<>|\[\]]", "'", text)[:300]


def write_outputs(path, report):
    emulators, sdks = canary_inputs(report)
    lines = [f"emulators={json.dumps(emulators, separators=(',', ':'))}",
             f"sdks={json.dumps(sdks, separators=(',', ':'))}",
             f"fingerprint={fingerprint(emulators, sdks)}",
             f"new={len(emulators) + len(sdks)}",
             f"errors={len(report['errors'])}"]
    with open(path, "a", encoding="utf-8") as f:
        f.write("\n".join(lines) + "\n")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("check", help="check the watch list against verified.rs and pinned.sh")
    run = sub.add_parser("detect", help="read the registries and report new releases")
    run.add_argument("--report", help="write the report (JSON) here")
    run.add_argument("--summary", help="append the Markdown summary here")
    run.add_argument("--outputs", help="append the canary's inputs here (GITHUB_OUTPUT)")
    run.add_argument("--emulator", nargs=2, action="append", default=[],
                     metavar=("VERSION", "SHA256"),
                     help="also give the canary this official emulator jar, new or not")
    args = parser.parse_args(argv)
    try:
        watch = load_watch()
        if args.command == "check":
            problems = check(watch, (ROOT / VERIFIED_RS).read_text(encoding="utf-8"),
                             (ROOT / PINNED_SH).read_text(encoding="utf-8"),
                             optional_json(SDK_CERTIFICATION), optional_json(COMPATIBILITY))
            if problems:
                raise Stop("\n".join(problems))
            print(f"{WATCH}: in step with what the repository says is verified.")
            return 0
        report = detect(watch, http_get, args.emulator)
    except Stop as err:
        print(f"error: {err}", file=sys.stderr)
        return 1
    if args.report:
        with open(args.report, "w", encoding="utf-8") as f:
            json.dump(report, f, indent=1, sort_keys=True)
            f.write("\n")
    text = summary(report)
    if args.summary:
        with open(args.summary, "a", encoding="utf-8") as f:
            f.write(text)
    else:
        print(text)
    if args.outputs:
        write_outputs(args.outputs, report)
    if os.environ.get("GITHUB_ACTIONS") == "true":
        for error in report["errors"]:
            print(f"::warning title=Upstream::{shown_line(error)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
