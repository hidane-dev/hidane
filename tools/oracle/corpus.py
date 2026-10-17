"""Writes the manifest of the Hidane Oracle Corpus (docs/oracle-corpus.md) from the recordings.

Usage (from the repository root):
    python3 tools/oracle/corpus.py           # writes docs/oracle-corpus.json
    python3 tools/oracle/corpus.py --check   # fails if it is stale or does not fit its schema

The corpus is every recording of the official emulator in the repository: the fixtures the
tests replay (`crates/*/tests/fixtures/*.json`, and the one in `results/`) and the official SDK
transcripts in `results/` that are compared with hidane's. Which files they are, their areas,
the tests that replay them, their answers and how each is compared come from
tools/parity/table.py (RECORDINGS, TRANSCRIPTS), which this script imports; it adds how each
was made (its recorder and the client that recorder used), the official emulator release it was
made against and how that is known, when it was first recorded and last changed, its size and
SHA-256, and totals.

A recording's release comes from its `oracle` header or its file name (`-official-v<x>.json`).
A fixture with neither gets it from git history: the single release `tools/oracle/` names at the
commit that last changed the fixture (or in the working tree, for a change not committed yet),
checked against the commits that changed it before. The dates come from git history too: the
first and the last commit that changed the file, in the time zone each was made in (today, for a
change not committed yet). These fields are kept as they are while the recording's SHA-256 stays
the same, so `--check` needs no history (CI checks out one commit) and a squash merge does not
make them stale. `--check` recomputes everything else, and validates the manifest against
docs/oracle-corpus.schema.json. Stdlib only.
"""

import hashlib
import json
import re
import subprocess
import sys
from collections import Counter
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "parity"))
import table  # noqa: E402  (tools/parity/table.py)

MANIFEST = "docs/oracle-corpus.json"
SCHEMA = "docs/oracle-corpus.schema.json"
SCHEMA_VERSION = 1
PRODUCT = "cloud-firestore-emulator"
README = "tools/oracle/README.md"
PINNED = ".github/actions/official-emulator/pinned.sh"
FIXTURE, TRANSCRIPT = "fixture", "transcript"
HEADER, FILE_NAME, HISTORY = "header", "file name", "git history"
PINNED_BY, RECORDED, UNPINNED = "pinned", "recorded", "unpinned"

# The interfaces a recorder uses on the official emulator. REST is any HTTP/1.1 request (the REST
# API and the emulator's own endpoints); process is the jar itself: the flags it starts with, what
# it prints, its exit code, the files it writes.
GRPC, REST, WEBCHANNEL, WEBSOCKET, PROCESS, CLI = (
    "gRPC", "REST", "WebChannel", "WebSocket", "process", "CLI")
INTERFACES = [GRPC, REST, WEBCHANNEL, WEBSOCKET, PROCESS, CLI]
PY, PY_GRPCURL = "Python standard library", "Python standard library and grpcurl"

# How each fixture's recorder talks to the official emulator, from its docstring and its code.
FIXTURE_CLIENTS = {
    "aggregations.py": ([REST], PY),
    "auth.py": ([GRPC, REST], PY_GRPCURL),
    "base_versions.py": ([REST], PY),
    "bulk_delete.py": ([REST], PY),
    "bson_types.py": ([REST], PY),
    "commit_checks.py": ([GRPC, REST], PY_GRPCURL),
    "conformance.py": ([REST], PY),
    "cors.py": ([REST], PY),
    "datastore_checks.py": ([REST], PY),
    "datastore_commit_checks.py": ([REST], PY),
    "datastore_entities.py": ([REST], PY),
    "datastore_export.py": ([REST, PROCESS], PY),
    "datastore_firestore_api.py": ([GRPC, REST], PY_GRPCURL),
    "datastore_gql.py": ([REST], PY),
    "datastore_gql_errors.py": ([REST], PY),
    "datastore_indexes.py": ([REST, PROCESS], PY),
    "datastore_locks.py": ([REST], PY),
    "datastore_projects.py": ([GRPC, REST, PROCESS], PY_GRPCURL),
    "datastore_protobuf.py": ([REST], PY),
    "datastore_queries.py": ([REST], PY),
    "datastore_sequences.py": ([REST], PY),
    "document_writes.py": ([REST], PY),
    "export_import.py": ([REST, PROCESS], PY),
    "field_order.py": ([REST], PY),
    "find_nearest.py": ([REST], PY),
    "function_events.py": ([GRPC, REST, PROCESS], PY_GRPCURL),
    "function_triggers.py": ([REST, PROCESS], PY),
    "grpc_settings.py": ([GRPC, REST], PY_GRPCURL),
    "index_usage.py": ([REST, WEBCHANNEL], PY),
    "json_escapes.py": ([REST, WEBCHANNEL], PY),
    "limits.py": ([REST], PY),
    "list_documents.py": ([GRPC, REST], PY_GRPCURL),
    "listen_queries.py": ([WEBCHANNEL, REST], PY),
    "negative_zero.py": ([GRPC, REST, WEBCHANNEL, PROCESS], PY_GRPCURL),
    "pipelines.py": ([REST], PY),
    "project_ids.py": ([REST], PY),
    "queries.py": ([REST], PY),
    "read_time_and_databases.py": ([GRPC, REST], PY_GRPCURL),
    "rest.py": ([REST], PY),
    "rules_auth.py": ([REST], PY),
    "rules_compile.py": ([REST], PY),
    "rules_conditions.py": ([REST], PY),
    "rules_coverage.py": ([REST, PROCESS], PY),
    "rules_denials.py": ([REST], PY),
    "rules_eval.py": ([REST], PY),
    "rules_expressions.py": ([REST], PY),
    "rules_expressions_grpc.py": ([GRPC, REST], PY_GRPCURL),
    "rules_expressions_monitor.py": ([REST, WEBSOCKET], PY),
    "rules_expressions_report.py": ([REST, PROCESS], PY),
    "rules_list.py": ([REST], PY),
    "rules_lookups.py": ([REST], PY),
    "rules_paths.py": ([REST], PY),
    "rules_request.py": ([REST], PY),
    "rules_same_document.py": ([REST], PY),
    "rules_request_fields.py": ([REST, PROCESS], PY),
    "rules_streams.py": ([GRPC, REST], PY_GRPCURL),
    "rules_verify.py": ([REST, PROCESS], PY),
    "rules_warnings.py": ([REST], PY),
    "rules_writes.py": ([REST], PY),
    "same_document_writes.py": ([REST], PY),
    "single_project_mode.py": ([GRPC, REST, WEBCHANNEL, PROCESS], PY_GRPCURL),
    "transactions.py": ([GRPC, REST], PY_GRPCURL),
    "transforms.py": ([REST], PY),
    "unsupported.py": ([GRPC, REST], PY_GRPCURL),
    "value_order.py": ([REST], PY),
    "webchannel.py": ([REST, WEBCHANNEL], PY),
    "websocket_escapes.py": ([REST, WEBSOCKET], PY),
    "websocket_requests.py": ([GRPC, REST, WEBSOCKET], PY_GRPCURL),
    "startup_flags.py": ([REST, PROCESS], PY),
    "write_stream.mjs": ([GRPC], "Node.js"),
}

# The recorder of each transcript pair in TRANSCRIPTS, where it is not tools/oracle/sdk_<name>.mjs.
TRANSCRIPT_RECORDERS = {
    "browser-rules": "tools/oracle/webchannel/rules_page.js",
    "browser-webchannel": "tools/oracle/webchannel/page.js",
    "export-import": "tools/oracle/sdk_export_import.mjs",
    "flutter-web": "tools/oracle/flutter_web/",
    "functions-triggers": "tools/oracle/functions/drive.mjs",
    "js-sdk-browser": "tools/js-sdk-browser/",
    "nodejs-system-tests": "tools/nodejs-system-tests/",
    "nodejs-system-tests-rest": "tools/nodejs-system-tests/",
}
for _sdk in ("android", "go", "ios", "java", "python"):
    for _section in ("documents", "listen", "queries", "transactions"):
        TRANSCRIPT_RECORDERS[f"sdk-{_sdk}-{_section}"] = f"tools/oracle/sdk_{_sdk}/"
for _sdk in ("java", "python"):
    for _section in ("entities", "queries", "transactions"):
        for _name in (f"sdk-datastore-{_sdk}-{_section}", f"sdk-datastore-{_sdk}-http-{_section}"):
            TRANSCRIPT_RECORDERS[_name] = f"tools/oracle/sdk_datastore_{_sdk}/"
for _section in ("entities", "queries", "transactions"):
    TRANSCRIPT_RECORDERS[f"sdk-datastore-go-{_section}"] = "tools/oracle/sdk_datastore_go/"
    TRANSCRIPT_RECORDERS[f"sdk-datastore-node-{_section}"] = "tools/oracle/sdk_datastore_node.mjs"

# Versions a recorder pins: (recorder, package, file, a pattern whose group 1 is the version).
PINS = [
    ("tools/oracle/sdk_go/", "cloud.google.com/go/firestore", "tools/oracle/sdk_go/go.mod",
     r"^\s*cloud\.google\.com/go/firestore v([\w.\-]+)$"),
    ("tools/oracle/sdk_datastore_go/", "cloud.google.com/go/datastore",
     "tools/oracle/sdk_datastore_go/go.mod", r"^\s*cloud\.google\.com/go/datastore v([\w.\-]+)$"),
    ("tools/oracle/sdk_python/", "google-cloud-firestore",
     "tools/oracle/sdk_python/requirements.txt", r"^google-cloud-firestore==([\w.\-]+)$"),
    ("tools/oracle/sdk_datastore_python/", "google-cloud-datastore",
     "tools/oracle/sdk_datastore_python/requirements.txt", r"^google-cloud-datastore==([\w.\-]+)$"),
    ("tools/oracle/sdk_java/", "com.google.cloud:google-cloud-firestore",
     "tools/oracle/sdk_java/pom.xml",
     r"<artifactId>google-cloud-firestore</artifactId>\s*<version>([\w.\-]+)</version>"),
    ("tools/oracle/sdk_datastore_java/", "com.google.cloud:google-cloud-datastore",
     "tools/oracle/sdk_datastore_java/pom.xml",
     r"<artifactId>google-cloud-datastore</artifactId>\s*<version>([\w.\-]+)</version>"),
    ("tools/js-sdk-browser/", "firebase-js-sdk", ".github/workflows/js-sdk-browser.yml",
     r"^\s*FIREBASE_JS_SDK_COMMIT: ([0-9a-f]{40})$"),
    ("tools/nodejs-system-tests/", "google-cloud-node", ".github/workflows/nodejs-system-tests.yml",
     r"^\s*GOOGLE_CLOUD_NODE_COMMIT: ([0-9a-f]{40})$"),
    ("tools/oracle/sdk_android/", "com.google.firebase:firebase-firestore",
     "tools/oracle/sdk_android/app/build.gradle.kts",
     r'"com\.google\.firebase:firebase-firestore:([\w.\-]+)"'),
    ("tools/oracle/sdk_ios/", "firebase-ios-sdk", "tools/oracle/sdk_ios/Package.swift",
     r'firebase-ios-sdk\.git", exact: "([\w.\-]+)"'),
] + [("tools/oracle/flutter_web/", name, "tools/oracle/flutter_web/pubspec.yaml",
      rf"^  {name}: (\d[\w.\-]*)$")
     for name in ("firebase_core", "cloud_firestore", "cloud_firestore_web", "firebase_core_web")]


class Stop(Exception):
    """A problem the person running the script has to fix."""


class Stale(Exception):
    """In --check: a recording the manifest does not list as it is now."""


CHECKING = False  # --check: never read git history


def source(rel):
    return (ROOT / rel).read_text(encoding="utf-8")


def git(*args):
    """Output of a git command in the repository; None when it fails (no match, no history)."""
    done = subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True)
    return done.stdout if done.returncode == 0 else None


def ordered(interfaces):
    return [name for name in INTERFACES if name in interfaces]


def readme_recorders():
    """Fixture -> its recorder, from the script table of tools/oracle/README.md."""
    found = {}
    for line in source(README).splitlines():
        if not line.startswith("| `"):
            continue
        cells = re.split(r"(?<!\\)\|", line)[1:-1]
        if len(cells) < 4:
            continue
        script = re.findall(r"`([^`]+)`", cells[0])[0]
        previous = None
        for token in re.findall(r"`([^`]+\.json)`", cells[2]):
            if "/" not in token and previous is None:
                continue
            path = token if "/" in token else f"{Path(previous).parent.as_posix()}/{token}"
            found[path] = f"tools/oracle/{script}"
            previous = path
    return found


def npm_packages(rel, seen=None):
    """The npm packages a script imports, following its relative imports."""
    seen = set() if seen is None else seen
    if rel in seen:
        return []
    seen.add(rel)
    names = []
    for spec in re.findall(r"""^\s*import\b[^'"]*?['"]([^'"]+)['"]""", source(rel), re.M):
        if spec.startswith("."):
            inner = (ROOT / Path(rel).parent / spec).resolve()
            found = npm_packages(inner.relative_to(ROOT).as_posix(), seen) if inner.is_file() \
                else []
        elif spec.startswith("node:") or spec in ("fs", "path", "url", "os", "child_process"):
            found = []
        else:
            parts = spec.split("/")
            found = ["/".join(parts[:2]) if spec.startswith("@") else parts[0]]
        names += [name for name in found if name not in names]
    return names


def packages(recorder, recording=None):
    """The packages a recorder used, each with its version where it pins it or the recording
    states it."""
    out = []
    for owner, name, rel, pattern in PINS:
        if owner == recorder:
            match = re.search(pattern, source(rel), re.M)
            if not match:
                raise Stop(f"{rel}: no pinned version of {name} found; update PINS.")
            out.append(dict(name=name, version=match.group(1), source=PINNED_BY, where=rel))
    if recorder.endswith((".mjs", ".js")):
        out += [dict(name=name, version=None, source=UNPINNED, where=None)
                for name in npm_packages(recorder)]
    stated = {}
    if isinstance(recording, list) and recording and isinstance(recording[0], dict):
        first = recording[0]
        if isinstance(first.get("versions"), dict):
            stated = first["versions"]  # sdk_rules_unit_testing.mjs and rules_page.js
        elif first.get("step") == "versions" and "firebaseJsSdk" in (first.get("value") or {}):
            stated = {"firebase-js-sdk": first["value"]["firebaseJsSdk"]}  # flutter_web
    for name, version in stated.items():
        entry = next((p for p in out if p["name"] == name), None)
        if entry is None:
            out.append(dict(name=name, version=version, source=RECORDED, where=None))
        elif entry["source"] == UNPINNED:
            entry.update(version=version, source=RECORDED)
    return out


def sha256(rel):
    return hashlib.sha256((ROOT / rel).read_bytes()).hexdigest()


def header_release(rel, data):
    """(release, jar SHA-256, source) from a recording's `oracle` header or file name."""
    header = data.get("oracle") if isinstance(data, dict) else None
    if header:
        match = re.fullmatch(rf"{PRODUCT} (v\d+(?:\.\d+)+) \(sha256 ([0-9a-f]{{64}})\)", header)
        if not match:
            raise Stop(f"{rel}: its `oracle` header {header!r} is not "
                       f"'{PRODUCT} v<version> (sha256 <jar SHA-256>)'.")
        return match.group(1), match.group(2), HEADER
    version = table.oracle_version(rel, data)
    if version:
        return version, None, FILE_NAME
    return None


NAMED = rf"{PRODUCT}[- ]v[0-9]+(\.[0-9]+)+"
NAMED_SHA = rf"{PRODUCT} v[0-9]+(\.[0-9]+)+ \(sha256 [0-9a-f]{{64}}\)"


def named_at(commit):
    """The releases tools/oracle/ names at a commit (None: in the working tree), each with the
    jar SHA-256s it gives them."""
    where = [commit] if commit else []
    found = {}
    for text in (git("grep", "-h", "-o", "-E", NAMED, *where, "--", "tools/oracle") or "").split():
        match = re.search(r"v\d+(?:\.\d+)+", text)
        if match:
            found.setdefault(match.group(0), set())
    out = git("grep", "-h", "-o", "-E", NAMED_SHA, *where, "--", "tools/oracle") or ""
    for version, sha in re.findall(r"(v\d+(?:\.\d+)+) \(sha256 ([0-9a-f]{64})\)", out):
        found.setdefault(version, set()).add(sha)
    return found


def history(rel):
    """The dates of the first and the last commit that changed a file, today's for a change not
    committed yet, and the commits."""
    log = [line.split() for line in (git("log", "--format=%H %ad", "--date=short", "--", rel)
                                     or "").splitlines() if line.strip()]
    head = (git("rev-parse", f"HEAD:{rel}") or "").strip()
    committed = bool(log) and head == (git("hash-object", rel) or "").strip()
    today = date.today().isoformat()
    first = log[-1][1] if log else today
    last = log[0][1] if committed else today
    return dict(first_recorded=first, last_changed=last), [sha for sha, _ in log], committed


def release_from_history(rel, commits, committed):
    """The release of a fixture without a header: the one tools/oracle/ names where the fixture's
    content was made, the commit that last changed it (or the working tree, for a change not
    committed yet), checked against the commits that changed it before."""
    current = named_at(commits[0] if committed else None)
    if len(current) != 1:
        raise Stop(f"{rel}: no `oracle` header, and tools/oracle/ names "
                   f"{sorted(current) or 'no release'} where it was last changed; give it a "
                   "header.")
    (version, shas), = current.items()
    before = commits[1:] if committed else commits
    earlier = set()
    for commit in before:
        earlier |= set(named_at(commit))
    if not committed:
        where = "in the working tree, where it changed"
    elif before:
        where = "at the commit that last changed it"
    else:
        where = "at the commit that added it"
    evidence = f"no header; tools/oracle/ names only {PRODUCT} {version} {where}"
    if before and earlier == {version}:
        count = "earlier commit" if len(before) == 1 else f"{len(before)} earlier commits"
        evidence += f", as at the {count} that changed it"
    elif before:
        evidence += f"; earlier commits that changed it name {', '.join(sorted(earlier))}"
    return dict(release=version, jar_sha256=next(iter(shas)) if len(shas) == 1 else None,
                source=HISTORY, evidence=evidence)


def provenance(rel, data, kept):
    """The release and dates of a recording. A release stated by its header or file name is read
    again; the dates, and a release found in git history, are kept from the manifest while the
    file is unchanged (`kept`), else taken from git history."""
    stated = header_release(rel, data)
    if kept is not None:
        oracle, dates = kept
        if stated is None and oracle.get("source") == HISTORY:
            return oracle, dates
        if stated is not None:
            version, jar, how = stated
            return dict(release=version, jar_sha256=jar, source=how), dates
    if kept is None and CHECKING:
        raise Stale(rel)
    dates, commits, committed = history(rel)
    if stated is not None:
        version, jar, how = stated
        return dict(release=version, jar_sha256=jar, source=how), dates
    return release_from_history(rel, commits, committed), dates


def load(rel):
    with open(ROOT / rel, encoding="utf-8") as f:
        return json.load(f)


def kept_fields(previous):
    """path -> (sha256, (oracle, dates)) from the committed manifest."""
    out = {}
    for entry in (previous or {}).get("recordings", []):
        out[entry["path"]] = (entry["sha256"], (entry["oracle"], entry["recorded"]))
    return out


def common(rel, kind, area, recorder, over, using, data, kept):
    if not (ROOT / recorder).exists():
        raise Stop(f"{rel}: its recorder {recorder} was not found.")
    digest = sha256(rel)
    previous = kept.get(rel)
    oracle, dates = provenance(rel, data, previous[1] if previous and previous[0] == digest
                               else None)
    return dict(path=rel, kind=kind, area=area, recorder=recorder,
                client=dict(over=ordered(over), using=using, packages=packages(recorder, data)),
                oracle=oracle, recorded=dates, bytes=(ROOT / rel).stat().st_size, sha256=digest)


def fixtures(kept):
    by_readme = readme_recorders()
    out = []
    for row in table.recordings():
        rel = row["path"]
        data = load(rel)
        named = data.get("generated_by") if isinstance(data, dict) else None
        recorder = named or by_readme.get(rel)
        if recorder is None:
            raise Stop(f"{rel}: no recorder known. Give it a `generated_by` header, or a row in "
                       f"the table of {README}.")
        if named and by_readme.get(rel) not in (None, named):
            raise Stop(f"{rel}: generated_by says {named}, {README} says {by_readme[rel]}.")
        script = Path(recorder).name
        if script not in FIXTURE_CLIENTS:
            raise Stop(f"{recorder}: not in FIXTURE_CLIENTS of tools/oracle/corpus.py; add how "
                       "it talks to the official emulator.")
        over, using = FIXTURE_CLIENTS[script]
        entry = common(rel, FIXTURE, row["area"], recorder, over, using, data, kept)
        answers = row["answers"]
        entry.update(
            answers=table.total(answers),
            levels={key: table.total(answers, key) for key, _ in table.LEVELS
                    if table.total(answers, key)},
            replayed_over=table.transports_of(answers),
            tests=row["tests"], counted=row["counted"], compared=row["compares"])
        out.append(entry)
    return out


def transcripts(kept):
    pairs = {}
    for pair in table.transcripts():
        pairs.setdefault(pair["name"], []).append(pair)
    out = []
    for name, rows in pairs.items():
        first = rows[0]
        rel = first["official"]
        script = f"tools/oracle/sdk_{name[4:].replace('-', '_')}.mjs" if name.startswith("sdk-") \
            else None
        recorder = TRANSCRIPT_RECORDERS.get(name, script)
        if recorder is None or not (ROOT / recorder).exists():
            raise Stop(f"results/{name}-*: no recorder known; add it to TRANSCRIPT_RECORDERS of "
                       "tools/oracle/corpus.py.")
        over = [part.strip() for part in first["transport"].split(",")]
        unknown = set(over) - set(INTERFACES)
        if unknown:
            raise Stop(f"TRANSCRIPTS[{name!r}]: transport {sorted(unknown)} unknown to corpus.py.")
        entry = common(rel, TRANSCRIPT, first["area"], recorder, over, first["client"], load(rel),
                       kept)
        entry.update(compared_with=[
            dict(path=row["other"], what=row["what"], result=row["result"],
                 **({"explained_in": row["where"]} if row["where"] else {}))
            for row in rows])
        out.append(entry)
    return out


def pinned():
    text = source(PINNED)
    version = re.search(r"^PINNED_VERSION=(\S+)$", text, re.M)
    sha = re.search(r"^PINNED_SHA256=([0-9a-f]{64})$", text, re.M)
    if not (version and sha):
        raise Stop(f"{PINNED}: PINNED_VERSION or PINNED_SHA256 not found.")
    return f"v{version.group(1)}", sha.group(1)


def manifest(previous):
    kept = kept_fields(previous)
    entries = fixtures(kept) + transcripts(kept)
    pin_version, pin_sha = pinned()
    releases = {}
    for entry in entries:
        oracle = entry["oracle"]
        shas = releases.setdefault(oracle["release"], set())
        if oracle.get("jar_sha256"):
            shas.add(oracle["jar_sha256"])
    releases.setdefault(pin_version, set()).add(pin_sha)
    for version, shas in releases.items():
        if len(shas) > 1:
            raise Stop(f"{version}: recordings name different jars ({', '.join(sorted(shas))}).")

    def count(select):
        chosen = [e for e in entries if select(e)]
        return dict(fixtures=sum(e["kind"] == FIXTURE for e in chosen),
                    transcripts=sum(e["kind"] == TRANSCRIPT for e in chosen),
                    answers=sum(e.get("answers", 0) for e in chosen))

    levels = Counter()
    for entry in entries:
        levels.update(entry.get("levels", {}))
    areas = [key for key in table.AREA_ORDER if any(e["area"] == key for e in entries)]
    by_path = sorted(entries, key=lambda e: e["path"])
    listing = "".join(f"{e['sha256']}  {e['path']}\n" for e in by_path)
    return {
        "$schema": "./oracle-corpus.schema.json",
        "name": "Hidane Oracle Corpus",
        "description": "The official Cloud Firestore emulator's answers that hidane records, "
                       "replays and compares: every recording, how it was made, the release it "
                       "was made against, and how its answers are compared. See "
                       "docs/oracle-corpus.md.",
        "schema_version": SCHEMA_VERSION,
        "generated_by": "tools/oracle/corpus.py",
        "digest": "sha256:" + hashlib.sha256(listing.encode()).hexdigest(),
        "releases": [dict(product=PRODUCT, release=version,
                          jar_sha256=next(iter(shas)) if shas else None,
                          pinned=version == pin_version)
                     for version, shas in sorted(releases.items())],
        "totals": {
            "files": len(entries),
            **count(lambda e: True),
            "bytes": sum(e["bytes"] for e in entries),
            "levels": {key: levels[key] for key, _ in table.LEVELS if levels[key]},
            "by_area": {key: count(lambda e, k=key: e["area"] == k) for key in areas},
            "by_release": {version: count(lambda e, v=version: e["oracle"]["release"] == v)
                           for version in sorted(releases)},
            "by_release_source": {how: sum(e["oracle"]["source"] == how for e in entries)
                                  for how in (HEADER, FILE_NAME, HISTORY)},
        },
        "recordings": entries,
    }


def dump(data):
    return json.dumps(data, indent=1, ensure_ascii=False) + "\n"


# A small JSON Schema validator: the keywords docs/oracle-corpus.schema.json uses.
TYPES = {"object": dict, "array": list, "string": str, "null": type(None)}


def is_type(value, kind):
    if kind == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if kind == "boolean":
        return isinstance(value, bool)
    return isinstance(value, TYPES[kind])


def problems(value, schema, root, where="$"):
    if "$ref" in schema:
        name = schema["$ref"].removeprefix("#/$defs/")
        return problems(value, root["$defs"][name], root, where)
    out = []
    kinds = schema.get("type")
    if kinds is not None:
        kinds = [kinds] if isinstance(kinds, str) else kinds
        if not any(is_type(value, kind) for kind in kinds):
            return [f"{where}: expected {' or '.join(kinds)}"]
    if "const" in schema and value != schema["const"]:
        out.append(f"{where}: expected {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        out.append(f"{where}: {value!r} not one of {schema['enum']}")
    if "pattern" in schema and isinstance(value, str) and not re.search(schema["pattern"], value):
        out.append(f"{where}: {value!r} does not match {schema['pattern']}")
    if "minimum" in schema and isinstance(value, int) and value < schema["minimum"]:
        out.append(f"{where}: below {schema['minimum']}")
    if isinstance(value, dict):
        for key in schema.get("required", []):
            if key not in value:
                out.append(f"{where}: {key} missing")
        props = schema.get("properties", {})
        for key, item in value.items():
            if "propertyNames" in schema:
                out += problems(key, schema["propertyNames"], root, f"{where} key {key!r}")
            if key in props:
                out += problems(item, props[key], root, f"{where}.{key}")
            elif schema.get("additionalProperties") is False:
                out.append(f"{where}: {key} not allowed")
            elif isinstance(schema.get("additionalProperties"), dict):
                out += problems(item, schema["additionalProperties"], root, f"{where}.{key}")
    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            out.append(f"{where}: fewer than {schema['minItems']} items")
        if schema.get("uniqueItems") and len({json.dumps(v, sort_keys=True) for v in value}) \
                != len(value):
            out.append(f"{where}: items repeat")
        if "items" in schema:
            for i, item in enumerate(value):
                out += problems(item, schema["items"], root, f"{where}[{i}]")
    if "oneOf" in schema:
        matching = [s for s in schema["oneOf"] if not problems(value, s, root, where)]
        if len(matching) != 1:
            out.append(f"{where}: matches {len(matching)} of oneOf")
    return out


def main():
    global CHECKING
    args = sys.argv[1:]
    if args not in ([], ["--check"]):
        raise Stop("usage: python3 tools/oracle/corpus.py [--check]")
    CHECKING = args == ["--check"]
    path = ROOT / MANIFEST
    previous = load(MANIFEST) if path.is_file() else None
    try:
        text = dump(manifest(previous))
    except Stale as stale:
        raise Stop(f"stale: {MANIFEST} does not have {stale} as it is. Run python3 "
                   "tools/oracle/corpus.py and commit the result.")
    schema = load(SCHEMA)
    found = problems(json.loads(text), schema, schema)
    if found:
        raise Stop(f"{MANIFEST} does not fit {SCHEMA}:\n  " + "\n  ".join(found[:20]))
    if path.is_file() and path.read_text(encoding="utf-8") == text:
        return
    if CHECKING:
        raise Stop(f"stale: {MANIFEST}. Run python3 tools/oracle/corpus.py and commit the "
                   "result.")
    path.write_text(text, encoding="utf-8")
    print(f"wrote {MANIFEST}")


if __name__ == "__main__":
    try:
        main()
    except (Stop, table.Stop) as stop:
        sys.exit(f"tools/oracle/corpus.py: {stop}")
