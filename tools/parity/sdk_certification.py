"""Generates the SDK certification matrix from the recorded results.

Usage (from the repository root):
    python3 tools/parity/sdk_certification.py           # writes the outputs below
    python3 tools/parity/sdk_certification.py --check   # fails if any is stale or invalid

Every client and tool hidane may be used with gets a row: its ecosystem and package, the transport
it uses against an emulator, the version that was run, what was exercised, a status and the
evidence. Nothing in a row comes from anywhere but the recorded results:

- the transcript pairs in `results/` (`<name>-official-v<version>.json` and `<name>-hidane.json`),
  which tools/parity/table.py lists (TRANSCRIPTS) and compares (identical, a documented difference,
  or unexplained), and which docs/oracle-corpus.json lists with their provenance;
- the version of a package a transcript ran with: recorded in the transcript itself, pinned by the
  script that made it (both from docs/oracle-corpus.json), or stated for that transcript by its
  row in results/README.md. A version none of them gives is not known;
- what a transcript exercised: EVIDENCE below declares it per client, and each declared feature
  must show in the names of the transcript's steps (or, where FEATURES says so, in its row in
  results/README.md), so a feature cannot be claimed for a transcript that does not have it.

A client is **verified** when every feature it is expected to offer (CLIENTS, `core`) was
exercised by a transcript pair that is identical or differs only as the documentation explains,
and the version it ran with is known; **partially verified** when some of that holds; **not
verified** when no recorded result covers it. Writes:

- `docs/sdk-certification.json`, validated against `docs/sdk-certification.schema.json`;
- the matrix in `docs/sdk-certification.md` and the table of verified versions in
  `docs/migrating.md`, between `<!-- sdk-certification:start -->` and `<!-- sdk-certification:end -->`;
- `VERIFIED` in `crates/hidane/src/project/verified.rs`, the versions `hidane doctor` judges a
  project's dependencies against, between `// sdk-certification:start` and `// sdk-certification:end`.

and checks that the status column of `docs/compatibility.md` says the same. Each recording's date is
the date of the last commit that changed it (today, for a change not committed yet), kept while
the file is unchanged, as tools/oracle/corpus.py keeps its dates; `--check` needs no history.
Stdlib only.
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "oracle"))
import corpus  # noqa: E402  (tools/oracle/corpus.py, which imports tools/parity/table.py)

table = corpus.table

OUTPUT = "docs/sdk-certification.json"
SCHEMA = "docs/sdk-certification.schema.json"
PAGE = "docs/sdk-certification.md"
MIGRATING = "docs/migrating.md"
COMPATIBILITY = "docs/compatibility.md"
VERIFIED_RS = "crates/hidane/src/project/verified.rs"
RESULTS_README = "results/README.md"
SCHEMA_VERSION = 1
START, END = "<!-- sdk-certification:start -->", "<!-- sdk-certification:end -->"
RS_START, RS_END = "// sdk-certification:start", "// sdk-certification:end"
ISSUES = "https://github.com/hidane-dev/hidane/issues"

VERIFIED, PARTIAL, NOT_VERIFIED = "verified", "partially-verified", "not-verified"
STATUSES = {
    VERIFIED: ("Verified", "Every feature the client is expected to offer was exercised by a "
                           "transcript pair whose two sides are identical or differ only as the "
                           "documentation explains, and the version it ran with is known."),
    PARTIAL: ("Partially verified", "Some recorded results cover the client, but a feature it is "
                                    "expected to offer was not exercised, or the version it ran "
                                    "with is not known, or a difference is unexplained."),
    NOT_VERIFIED: ("Not verified", "No recorded result covers the client. hidane may serve its "
                                   "transport; nobody has compared its results yet."),
}
PINNED, RECORDED, STATED, UNKNOWN = "pinned", "recorded", "stated", "unknown"

# What a client can be exercised on: key, label, a pattern the names of a transcript's steps must
# match for the feature to be claimed (None: no step names say it), and a pattern its row in
# results/README.md must match instead.
FEATURES = [
    ("documents", "documents and writes",
     r"\bset|update|delete|write|batch|\bget|\badd\b|create", None),
    ("queries", "queries", r"quer|where|orderBy|limit|collectionGroup|getDocs|\blist|cursor",
     None),
    ("aggregations", "aggregation queries", r"count|\bsum\b|average|aggregat", None),
    ("vector-search", "vector search", r"findNearest|vector", None),
    ("transactions", "transactions", r"transaction", None),
    ("listeners", "listeners", r"listen|snapshot|stream", None),
    ("reconnect", "going offline and resuming", r"offline|online", None),
    ("security-rules", "Security Rules as signed-in, signed-out and rules-disabled users",
     r"rules|signed[ -]out", None),
    ("clear", "clearing data", r"clear|reset|recursive", None),
    ("triggers", "Firestore triggers, v1 and v2", r"^v[12]|firestore\.document", None),
    ("export-import", "export and import", None, r"--import|--export-on-exit"),
    ("launch", "launch under firebase emulators:exec", None, r"emulators:exec"),
]
FEATURE_ORDER = [key for key, _, _, _ in FEATURES]
FEATURE_LABEL = {key: label for key, label, _, _ in FEATURES}
FEATURE_PATTERN = {key: (steps, row) for key, _, steps, row in FEATURES}

GRPC, REST, WEBCHANNEL, CLI = table.GRPC, table.REST, table.WEBCHANNEL, table.CLI
SDK = ["documents", "queries", "aggregations", "transactions", "listeners"]

# Every client and tool, in the order of the matrix. `core`: the features it must have been
# exercised on to be verified. `scope`: how `hidane doctor` names the row when its package has
# others. `compatibility`: the start of its row in docs/compatibility.md, whose status column must
# agree. `tracking`: the issue that covers verifying it.
CLIENTS = [
    dict(id="firebase-js-node", client="firebase-js-sdk imported in Node", ecosystem="npm",
         package="firebase", platform="Node.js", transport=[GRPC], core=SDK,
         scope="in Node over gRPC", compatibility="firebase-js-sdk imported **in Node**"),
    dict(id="firebase-js-lite", client="firebase-js-sdk Lite", ecosystem="npm",
         package="firebase", platform="Node.js", transport=[REST],
         core=["documents", "queries", "aggregations", "transactions"], scope="Lite over REST",
         compatibility="firebase-js-sdk **Lite**"),
    dict(id="firebase-js-browser", client="firebase-js-sdk in the browser", ecosystem="npm",
         package="firebase", platform="Chromium", transport=[WEBCHANNEL, REST], core=SDK,
         scope="in Chromium over WebChannel",
         compatibility="firebase-js-sdk **in the browser**"),
    dict(id="admin-node", client="firebase-admin (Node) / @google-cloud/firestore",
         ecosystem="npm", package="@google-cloud/firestore", platform="Node.js",
         transport=[GRPC], core=SDK, scope="over gRPC, with the default settings",
         compatibility="firebase-admin (Node) / `@google-cloud/firestore` (default settings)"),
    dict(id="admin-node-prefer-rest", client="@google-cloud/firestore with preferRest",
         ecosystem="npm", package="@google-cloud/firestore", platform="Node.js",
         transport=[REST, GRPC], core=SDK, scope="with `preferRest`",
         compatibility="`@google-cloud/firestore` with `preferRest: true`"),
    dict(id="rules-unit-testing", client="@firebase/rules-unit-testing", ecosystem="npm",
         package="@firebase/rules-unit-testing", platform="Node.js, Chromium",
         transport=[GRPC, WEBCHANNEL, REST], core=["security-rules", "clear"],
         scope="in Node and in Chromium"),
    dict(id="firebase-tools", client="firebase-tools", ecosystem="npm", package="firebase-tools",
         platform="Node.js", transport=[CLI], core=["launch", "export-import"],
         scope="through `hidane exec`"),
    dict(id="firebase-functions", client="firebase-functions under the Functions emulator",
         ecosystem="npm", package="firebase-functions", platform="Node.js", transport=[CLI],
         core=["triggers"], scope="under the Functions emulator"),
    dict(id="python", client="google-cloud-firestore (Python)", ecosystem="pypi",
         package="google-cloud-firestore", platform="Python", transport=[GRPC], core=SDK,
         scope="over gRPC", compatibility="google-cloud-firestore (Python)"),
    dict(id="go", client="google-cloud-go firestore", ecosystem="go",
         package="cloud.google.com/go/firestore", platform="Go", transport=[GRPC], core=SDK,
         scope="over gRPC", compatibility="google-cloud-go `firestore`"),
    dict(id="java", client="java-firestore", ecosystem="maven",
         package="com.google.cloud:google-cloud-firestore", platform="JVM", transport=[GRPC],
         core=SDK, scope="over gRPC", compatibility="java-firestore"),
    dict(id="flutter-web", client="FlutterFire cloud_firestore on the web", ecosystem="pub",
         package="cloud_firestore", platform="Chromium", transport=[WEBCHANNEL, REST], core=SDK,
         scope="Flutter Web in Chromium", compatibility="FlutterFire `cloud_firestore_web`"),
    dict(id="flutter-native", client="FlutterFire cloud_firestore on Android, iOS, macOS, Windows",
         ecosystem="pub", package="cloud_firestore", platform="Android, iOS, macOS, Windows",
         transport=[GRPC], core=SDK, scope="on Android, iOS, macOS and Windows",
         compatibility="FlutterFire `cloud_firestore` on Android", tracking=187),
    dict(id="ios", client="firebase-ios-sdk", ecosystem="swiftpm", package="FirebaseFirestore",
         platform="iOS, macOS, tvOS, watchOS", transport=[GRPC], core=SDK, scope="over gRPC",
         compatibility="firebase-ios-sdk", tracking=187),
    dict(id="android", client="firebase-android-sdk", ecosystem="maven",
         package="com.google.firebase:firebase-firestore", platform="Android", transport=[GRPC],
         core=SDK, scope="over gRPC", compatibility="firebase-android-sdk", tracking=187),
    dict(id="cpp-unity", client="Firebase C++ SDK / Unity", ecosystem="other",
         package="firebase-cpp-sdk", platform="iOS, Android, desktop", transport=[GRPC],
         core=SDK, scope="over gRPC", compatibility="Firebase C++ SDK / Unity"),
]

# What each transcript pair exercised, per client: name in TRANSCRIPTS -> {client id: features}.
# Every transcript needs an entry ({} when it covers no client: say why next to it).
EVIDENCE = {
    "browser-rules": {"firebase-js-browser": ["documents", "queries", "listeners",
                                              "transactions"],
                      "rules-unit-testing": ["security-rules"]},
    "browser-webchannel": {"firebase-js-browser": ["documents", "queries", "listeners"]},
    "export-import": {"firebase-tools": ["export-import", "launch"]},
    "flutter-web": {"flutter-web": ["documents", "queries", "aggregations", "transactions",
                                    "listeners"]},
    "functions-triggers": {"firebase-functions": ["triggers"], "firebase-tools": ["launch"]},
    "js-sdk-browser": {"firebase-js-browser": ["documents", "queries", "aggregations",
                                               "transactions", "listeners"]},
    "nodejs-system-tests": {"admin-node": ["documents", "queries", "aggregations",
                                           "vector-search", "transactions", "listeners",
                                           "clear"]},
    "sdk-aggregations": {"admin-node": ["aggregations"]},
    "sdk-auth": {"rules-unit-testing": ["security-rules"]},
    "sdk-clear": {"rules-unit-testing": ["clear"]},
    "sdk-documents": {"admin-node": ["documents"]},
    "sdk-find-nearest": {"admin-node": ["vector-search"], "firebase-js-node": ["documents"]},
    "sdk-listen": {"admin-node": ["listeners"],
                   "firebase-js-node": ["documents", "queries", "listeners"]},
    "sdk-listen-resume": {"firebase-js-node": ["listeners", "reconnect"]},
    "sdk-queries": {"admin-node": ["queries"]},
    "sdk-rest": {"firebase-js-lite": ["documents", "queries", "aggregations", "transactions"]},
    "sdk-rules-unit-testing": {"firebase-js-node": ["documents", "queries", "listeners",
                                                    "transactions"],
                               "rules-unit-testing": ["security-rules", "clear"]},
    "sdk-transactions": {"admin-node": ["transactions"]},
    "sdk-web-aggregations": {"firebase-js-node": ["aggregations"]},
    "sdk-web-transactions": {"firebase-js-node": ["transactions"]},
    "sdk-web-writes": {"firebase-js-node": ["documents"]},
}
for _sdk in ("go", "java", "python"):
    EVIDENCE[f"sdk-{_sdk}-documents"] = {_sdk: ["documents"]}
    EVIDENCE[f"sdk-{_sdk}-queries"] = {_sdk: ["queries", "aggregations", "vector-search"]}
    EVIDENCE[f"sdk-{_sdk}-transactions"] = {_sdk: ["transactions"]}
    EVIDENCE[f"sdk-{_sdk}-listen"] = {_sdk: ["listeners"]}

# How docs/migrating.md names each package hidane doctor judges.
PACKAGE_LABELS = {
    ("npm", "firebase"): "`firebase` (JS SDK)",
    ("npm", "@google-cloud/firestore"): "`@google-cloud/firestore` (the Firestore client of "
                                        "`firebase-admin`)",
    ("npm", "@firebase/rules-unit-testing"): "`@firebase/rules-unit-testing`",
    ("npm", "firebase-tools"): "`firebase-tools`",
    ("npm", "firebase-functions"): "`firebase-functions` (Firestore triggers under the "
                                   "Functions emulator)",
    ("pypi", "google-cloud-firestore"): "`google-cloud-firestore` (Python)",
    ("go", "cloud.google.com/go/firestore"): "`cloud.google.com/go/firestore`",
    ("maven", "com.google.cloud:google-cloud-firestore"):
        "`com.google.cloud:google-cloud-firestore` (Java)",
    ("pub", "cloud_firestore"): "`cloud_firestore` (Flutter)",
}

RESULT_ORDER = [table.IDENTICAL, table.DOCUMENTED, table.UNEXPLAINED]
OK_RESULTS = (table.IDENTICAL, table.DOCUMENTED)


class Stop(Exception):
    """A problem the person running the script has to fix."""


def source(rel):
    return (ROOT / rel).read_text(encoding="utf-8")


def load(rel):
    with open(ROOT / rel, encoding="utf-8") as f:
        return json.load(f)


def readme_rows():
    """Transcript name -> its row in the table of results/README.md. A row names a pair
    (`<name>-official-v<x>.json`) or a family (`<name>-<section>-official-v<x>.json`, for the
    sections `a`, `b` and `c`)."""
    rows = {}
    for line in source(RESULTS_README).splitlines():
        if not line.startswith("| `"):
            continue
        first = line.split(" | ")[0]
        names = re.findall(r"`([\w-]+)-official-v[\d.]+\.json`", first)
        family = re.findall(r"`([\w-]+)-<section>-official-v[\d.]+\.json`", first)
        sections = re.findall(r"`(\w+)`", first.split("for the sections", 1)[1]) \
            if "for the sections" in first else []
        names += [f"{prefix}-{section}" for prefix in family for section in sections]
        for name in names:
            rows.setdefault(name, line)
    return rows


def stated_version(row, package):
    """The version of a package a row of results/README.md states, or None."""
    pattern = rf"(?<![\w@/.-])`?{re.escape(package)}`? v?(\d+(?:\.\d+)+)\b"
    found = set(re.findall(pattern, row or ""))
    if len(found) > 1:
        raise Stop(f"{RESULTS_README}: one row states {package} {', '.join(sorted(found))}.")
    return found.pop() if found else None


def step_names(data):
    """The names a transcript gives its steps: `name`, `step`, a trigger and its event type; a
    suite's passed tests; a transcript that is an object of scenarios, their names."""
    if isinstance(data, dict) and isinstance(data.get("tests"), dict):
        return [test for test, outcome in data["tests"].items() if outcome == "passed"]
    if isinstance(data, dict):
        return list(data)
    names = []
    for item in data if isinstance(data, list) else []:
        if isinstance(item, dict):
            names += [str(item[key]) for key in ("name", "step", "trigger", "eventType")
                      if item.get(key) is not None]
    return names


def check_features(name, client, features, names, row):
    for feature in features:
        if feature not in FEATURE_LABEL:
            raise Stop(f"EVIDENCE[{name!r}][{client!r}]: unknown feature {feature!r}.")
        steps, readme = FEATURE_PATTERN[feature]
        if steps and any(re.search(steps, step, re.I) for step in names):
            continue
        if readme and row and re.search(readme, row):
            continue
        where = "its step names" if steps else f"its row in {RESULTS_README}"
        raise Stop(f"EVIDENCE[{name!r}][{client!r}]: {feature!r} is not visible in {where}; "
                   "claim only what the transcript exercised.")


def packages_of(entry, row, wanted):
    """The packages a transcript ran with, each with its version and how that is known: the
    corpus's (pinned by the recorder, or recorded in the transcript), else the version its row in
    results/README.md states, else unknown. `wanted` adds packages the corpus does not list."""
    out = []
    names = [p["name"] for p in entry["client"]["packages"]]
    for package in entry["client"]["packages"] + [dict(name=n, version=None, source=None,
                                                        where=None)
                                                   for n in wanted if n not in names]:
        name, version = package["name"], package["version"]
        stated = stated_version(row, name)
        if version is not None and package["source"] in (corpus.PINNED_BY, corpus.RECORDED):
            if stated is not None and stated != version:
                raise Stop(f"{entry['path']}: {name} {version} ({package['source']}), but "
                           f"{RESULTS_README} states {stated}.")
            out.append(dict(name=name, version=version, source=package["source"],
                            where=package["where"] or entry["path"]))
        elif stated is not None:
            out.append(dict(name=name, version=stated, source=STATED, where=RESULTS_README))
        else:
            out.append(dict(name=name, version=None, source=UNKNOWN, where=None))
    return out


def kept_dates(previous):
    """path -> (sha256, recorded) of every compared transcript in the committed output."""
    kept = {}
    for client in (previous or {}).get("clients", []):
        for evidence in client["evidence"]:
            for compared in evidence["compared"]:
                kept[compared["path"]] = (compared["sha256"], compared["recorded"])
    return kept


def dates(rel, kept):
    digest = corpus.sha256(rel)
    previous = kept.get(rel)
    if previous and previous[0] == digest:
        return digest, previous[1]
    if corpus.CHECKING:
        raise corpus.Stale(rel)
    recorded, _, _ = corpus.history(rel)
    return digest, recorded


def evidence(kept):
    """Transcript name -> what it holds, from the corpus manifest and results/README.md."""
    manifest = {entry["path"]: entry for entry in load(corpus.MANIFEST)["recordings"]
                if entry["kind"] == corpus.TRANSCRIPT}
    names = {pair["name"] for pair in table.transcripts()}
    missing, unknown = sorted(names - set(EVIDENCE)), sorted(set(EVIDENCE) - names)
    if missing:
        raise Stop(f"{', '.join(missing)}: not in EVIDENCE. Say which clients each transcript "
                   "exercised and on what ({} for none), then run the script.")
    if unknown:
        raise Stop(f"{', '.join(unknown)}: in EVIDENCE but not a transcript pair of "
                   "tools/parity/table.py; remove the entries.")
    ids = {client["id"]: client for client in CLIENTS}
    rows = readme_rows()
    out = {}
    for name in sorted(names):
        entry = next((e for path, e in manifest.items()
                      if re.fullmatch(rf"results/{re.escape(name)}-official-v[\d.]+\.json",
                                      path)), None)
        if entry is None:
            raise Stop(f"{corpus.MANIFEST} has no official transcript of {name}; run "
                       "python3 tools/oracle/corpus.py first.")
        row = rows.get(name)
        if row is None:
            raise Stop(f"{RESULTS_README}: no row names {name}-official-v<version>.json.")
        names_ = step_names(load(entry["path"]))
        for client, features in EVIDENCE[name].items():
            if client not in ids:
                raise Stop(f"EVIDENCE[{name!r}]: unknown client {client!r}.")
            check_features(name, client, features, names_, row)
        wanted = [ids[client]["package"] for client in EVIDENCE[name]]
        compared = []
        for other in entry["compared_with"]:
            digest, recorded = dates(other["path"], kept)
            compared.append(dict(path=other["path"], result=other["result"],
                                 **({"explained_in": other["explained_in"]}
                                    if "explained_in" in other else {}),
                                 sha256=digest, recorded=recorded))
        result = max((c["result"] for c in compared), key=RESULT_ORDER.index)
        out[name] = dict(transcript=name, official=entry["path"],
                         official_emulator=entry["oracle"]["release"],
                         official_recorded=entry["recorded"], compared=compared, result=result,
                         packages=packages_of(entry, row, wanted))
    return out


def ordered(features):
    return [key for key in FEATURE_ORDER if key in features]


def clients(transcripts):
    out = []
    for client in CLIENTS:
        cid, package = client["id"], client["package"]
        found = []
        for name, by_client in EVIDENCE.items():
            if cid in by_client:
                found.append(dict(transcripts[name], features=ordered(by_client[cid])))
        found.sort(key=lambda e: e["transcript"])
        exercised = ordered({f for e in found if e["result"] in OK_RESULTS for f in e["features"]})
        unexplained = ordered({f for e in found if e["result"] == table.UNEXPLAINED
                               for f in e["features"]} - set(exercised))
        versions = sorted({p["version"] for e in found for p in e["packages"]
                           if p["name"] == package and p["version"] is not None})
        if len(versions) > 1:
            raise Stop(f"{cid}: the recorded results ran {package} {', '.join(versions)}. "
                       "Record the others again with one version, or give each its own row.")
        version = versions[0] if versions else None
        not_exercised = [f for f in client["core"] if f not in exercised]
        gaps = []
        if found:
            gaps += [f"{FEATURE_LABEL[f]} not compared" for f in not_exercised]
            gaps += [f"{FEATURE_LABEL[f]}: an unexplained difference" for f in unexplained]
            if version is None:
                gaps.append(f"the version of {package} is not stated by any recorded result")
        if not found:
            status = NOT_VERIFIED
        elif not gaps:
            status = VERIFIED
        else:
            status = PARTIAL
        releases = sorted({e["official_emulator"] for e in found})
        last = max((c["recorded"]["last_changed"] for e in found for c in e["compared"]),
                   default=None)
        out.append(dict(
            id=cid, client=client["client"], ecosystem=client["ecosystem"], package=package,
            platform=client["platform"], transport=client["transport"], status=status,
            version=version, official_emulator=releases, core=client["core"],
            exercised=exercised, not_exercised=not_exercised, gaps=gaps, last_recorded=last,
            tracking=f"{ISSUES}/{client['tracking']}" if client.get("tracking") else None,
            evidence=found))
    return out


def joined(parts):
    if len(parts) < 3:
        return " and ".join(parts)
    return ", ".join(parts[:-1]) + ", and " + parts[-1]


def verified_packages(rows):
    """What hidane doctor judges a dependency against: each package with a verified row, the
    version it was verified with and what was verified."""
    by_package = {}
    for row in rows:
        by_package.setdefault((row["ecosystem"], row["package"]), []).append(row)
    out = []
    for (ecosystem, name), group in by_package.items():
        counted = [row for row in group if row["status"] != NOT_VERIFIED]
        if not any(row["status"] == VERIFIED for row in group):
            continue
        versions = {row["version"] for row in counted if row["version"] is not None}
        if len(versions) != 1:
            raise Stop(f"{name}: its rows ran {', '.join(sorted(versions))}; hidane doctor needs "
                       "one verified version.")
        scope = {row["id"]: next(c["scope"] for c in CLIENTS if c["id"] == row["id"])
                 for row in counted}
        features = lambda row: ", ".join(FEATURE_LABEL[f] for f in row["exercised"])
        if len(counted) == 1 and len(group) == 1:
            what = features(counted[0])
        elif len(counted) == 1:
            what = f"{scope[counted[0]['id']]}: {features(counted[0])}"
        else:
            # The verified rows first, then the partially verified ones.
            what = joined([scope[row["id"]] for row in counted if row["status"] == VERIFIED]
                          + [f"partially {scope[row['id']]}" for row in counted
                             if row["status"] == PARTIAL])
        out.append(dict(ecosystem=ecosystem, name=name, version=versions.pop(), what=what,
                        source=PAGE, clients=[row["id"] for row in counted]))
    return out


def output(previous):
    transcripts = evidence(kept_dates(previous))
    rows = clients(transcripts)
    totals = {"clients": len(rows)}
    totals.update({status: sum(row["status"] == status for row in rows) for status in STATUSES})
    return {
        "$schema": "./sdk-certification.schema.json",
        "name": "hidane SDK certification matrix",
        "description": "Every client and tool hidane may be used with: the version that was run, "
                       "the transport, what was exercised, a status and the recorded results "
                       "behind it. Generated from results/ only. See docs/sdk-certification.md.",
        "schema_version": SCHEMA_VERSION,
        "generated_by": "tools/parity/sdk_certification.py",
        "statuses": [dict(key=key, label=label, meaning=meaning)
                     for key, (label, meaning) in STATUSES.items()],
        "features": [dict(key=key, label=label) for key, label, _, _ in FEATURES],
        "totals": totals,
        "clients": rows,
        "verified_packages": verified_packages(rows),
    }


def link(rel, base="../"):
    return f"[`{Path(rel).name}`]({base}{rel})"


def version_cell(row):
    if row["version"] is None:
        return "not known" if row["evidence"] else "—"
    return row["version"]


def evidence_cell(row):
    if not row["evidence"]:
        return f"none ([#{row['tracking'].rsplit('/', 1)[1]}]({row['tracking']}))" \
            if row["tracking"] else "none"
    parts = []
    for e in row["evidence"]:
        hidane = next(c for c in e["compared"] if c["path"].endswith("-hidane.json"))
        result = e["result"] if e["result"] != table.DOCUMENTED else "documented difference"
        parts.append(f"[{e['transcript']}](../{hidane['path']}) ({result})")
    return ", ".join(parts)


def exercised_cell(row):
    cell = ", ".join(FEATURE_LABEL[f] for f in row["exercised"]) or "—"
    if row["evidence"] and row["not_exercised"]:
        cell += "; **not compared:** " + ", ".join(FEATURE_LABEL[f] for f in row["not_exercised"])
    return cell


def how_known(row):
    """`13.0.0: recorded in a, b; stated for c; not stated for d`."""
    by_source = {}
    for e in row["evidence"]:
        for p in e["packages"]:
            if p["name"] == row["package"]:
                by_source.setdefault(p["source"], []).append(f"`{e['transcript']}`")
    labels = [(RECORDED, "recorded in the transcripts of"),
              (PINNED, "pinned by the script that made"),
              (STATED, f"stated by [results/README.md](../{RESULTS_README}) for"),
              (UNKNOWN, "not stated for")]
    return "; ".join(f"{label} {', '.join(by_source[key])}" for key, label in labels
                     if key in by_source)


def markdown(data):
    rows, totals = data["clients"], data["totals"]
    releases = sorted({r for row in rows for r in row["official_emulator"]})
    out = [START, "<!-- Generated by tools/parity/sdk_certification.py; edit the script, not "
                  "this. -->", ""]
    out.append(
        f"**{totals['clients']} clients and tools**: {totals[VERIFIED]} verified, "
        f"{totals[PARTIAL]} partially verified, {totals[NOT_VERIFIED]} not verified, against the "
        f"official emulator {', '.join(releases)}.")
    out += ["", "| Client | Package | Version | Transport | Exercised | Status | Evidence "
                "| Last recorded |", "|---|---|---|---|---|---|---|---|"]
    for row in rows:
        client = row["client"] if row["platform"] in row["client"] \
            else f"{row['client']} ({row['platform']})"
        out.append(
            f"| {client} | {row['ecosystem']} `{row['package']}` | "
            f"{version_cell(row)} | {', '.join(row['transport'])} | {exercised_cell(row)} | "
            f"{STATUSES[row['status']][0]} | {evidence_cell(row)} | "
            f"{row['last_recorded'] or '—'} |")
    out += ["", "### Why a client is not verified in full", ""]
    for row in rows:
        if row["status"] == VERIFIED:
            continue
        why = "; ".join(row["gaps"]) if row["evidence"] else "no recorded result covers it"
        out.append(f"- {row['client']} ({STATUSES[row['status']][0].lower()}): {why}.")
    out += ["", "### How each version is known", ""]
    for row in rows:
        if row["evidence"]:
            known = row["version"] or "not known"
            out.append(f"- {row['client']}, `{row['package']}` {known}: {how_known(row)}.")
    out += ["", "### Versions `hidane doctor` judges against", "",
            "| Package | Version | What was verified |", "|---|---|---|"]
    for package in data["verified_packages"]:
        out.append(f"| {package['ecosystem']} `{package['name']}` | {package['version']} | "
                   f"{package['what']} |")
    out += ["", END]
    return "\n".join(out)


def migrating(data):
    rows = {row["id"]: row for row in data["clients"]}
    out = [START, "<!-- Generated by tools/parity/sdk_certification.py from "
                  "docs/sdk-certification.json; edit the script, not this. -->", "",
           "| Package | Verified with | Source |", "|---|---|---|"]
    for package in data["verified_packages"]:
        key = (package["ecosystem"], package["name"])
        if key not in PACKAGE_LABELS:
            raise Stop(f"{package['name']}: add how docs/migrating.md names it to PACKAGE_LABELS.")
        names = {e["transcript"] for cid in package["clients"] for e in rows[cid]["evidence"]}
        files = ", ".join(f"`results/{glob}-*.json`" for glob in families(names))
        out.append(f"| {PACKAGE_LABELS[key]} | {package['version']} — {package['what']} | "
                   f"[sdk-certification.md](sdk-certification.md), {files} |")
    out += ["", END]
    return "\n".join(out)


def families(names):
    """Transcript names, those of a family (`sdk-go-documents`, `sdk-go-queries`, …) as one
    `sdk-go` when all of them are among the names."""
    every = set(EVIDENCE)
    out = set()
    for name in names:
        family = "-".join(name.split("-")[:2])
        members = {n for n in every if n.startswith(family + "-")}
        out.add(family if len(members) > 1 and members <= set(names) and family not in every
                else name)
    return sorted(out)


def rust_string(text):
    if any(ord(c) < 0x20 for c in text):
        raise Stop(f"{text!r}: control characters do not belong in verified.rs.")
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"') + '"'


def rust(data):
    out = [RS_START,
           "// Generated by tools/parity/sdk_certification.py from the recorded results",
           "// (docs/sdk-certification.json); edit the script, not this.",
           "pub const VERIFIED: &[Verified] = &["]
    for package in data["verified_packages"]:
        out += ["    Verified {"]
        for field in ("ecosystem", "name", "version", "what", "source"):
            out.append(f"        {field}: {rust_string(package[field])},")
        out += ["    },"]
    out += ["];", RS_END]
    return "\n".join(out)


def between(rel, block, start, end):
    text = source(rel)
    if text.count(start) != 1 or text.count(end) != 1 or text.index(start) > text.index(end):
        raise Stop(f"{rel}: expected one {start} before one {end}.")
    return text[:text.index(start)] + block + text[text.index(end) + len(end):]


def check_compatibility(data):
    """The status column of docs/compatibility.md says what the matrix says."""
    lines = [line for line in source(COMPATIBILITY).splitlines() if line.startswith("| ")]
    for row in data["clients"]:
        prefix = next(c.get("compatibility") for c in CLIENTS if c["id"] == row["id"])
        if prefix is None:
            continue
        line = next((line for line in lines if line[2:].startswith(prefix)), None)
        if line is None:
            raise Stop(f"{COMPATIBILITY}: no row starts with {prefix!r}; update CLIENTS.")
        status = line.rstrip(" |").rsplit(" | ", 1)[1]
        label = STATUSES[row["status"]][0].lower()
        if not re.match(rf"{re.escape(label)}\b", status):
            raise Stop(f"{COMPATIBILITY}: the row of {row['client']} says {status!r}; the matrix "
                       f"says {label} ({PAGE}).")


def outputs(previous):
    data = output(previous)
    schema = load(SCHEMA)
    found = corpus.problems(data, schema, schema)
    if found:
        raise Stop(f"{OUTPUT} does not fit {SCHEMA}:\n  " + "\n  ".join(found[:20]))
    check_compatibility(data)
    return {
        OUTPUT: json.dumps(data, indent=1, ensure_ascii=False) + "\n",
        PAGE: between(PAGE, markdown(data), START, END),
        MIGRATING: between(MIGRATING, migrating(data), START, END),
        VERIFIED_RS: between(VERIFIED_RS, rust(data), RS_START, RS_END),
    }


def main():
    args = sys.argv[1:]
    if args not in ([], ["--check"]):
        raise Stop("usage: python3 tools/parity/sdk_certification.py [--check]")
    corpus.CHECKING = args == ["--check"]
    previous = load(OUTPUT) if (ROOT / OUTPUT).is_file() else None
    try:
        texts = outputs(previous)
    except corpus.Stale as stale:
        raise Stop(f"stale: {OUTPUT} does not have {stale} as it is. Run python3 "
                   "tools/parity/sdk_certification.py and commit the result.")
    stale = []
    for rel, text in texts.items():
        path = ROOT / rel
        if path.is_file() and path.read_text(encoding="utf-8") == text:
            continue
        stale.append(rel)
        if not corpus.CHECKING:
            path.write_text(text, encoding="utf-8")
    if corpus.CHECKING and stale:
        raise Stop(f"stale: {', '.join(stale)}. Run python3 tools/parity/sdk_certification.py "
                   "and commit the result.")
    for rel in stale:
        print(f"wrote {rel}")


if __name__ == "__main__":
    try:
        main()
    except (Stop, corpus.Stop, table.Stop) as stop:
        sys.exit(f"tools/parity/sdk_certification.py: {stop}")
