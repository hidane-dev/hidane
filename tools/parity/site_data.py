"""Writes the public data hidane.dev can show: compatibility, benchmarks and adopters.

Usage (from the repository root):
    python3 tools/parity/site_data.py           # writes website/public/data/*.json
    python3 tools/parity/site_data.py --check   # fails if one is stale or ADOPTERS.md invalid

Three files, served as https://hidane.dev/data/<name>.json for pages such as /compatibility,
/benchmarks and /adopters to read (no page reads them yet):

- `compatibility.json`: the public summary of docs/compatibility.json, docs/sdk-certification.json
  and docs/parity-exceptions.json: the matrix hidane × firebase-tools × official emulator, each
  client's status, version, what was exercised and its evidence, the parity counts and the
  differences by kind. Nothing in it comes from anywhere but those files;
- `benchmarks.json`: where the canonical benchmark's results are (hidane-dev/benchmark-results and
  its index.json, which a page fetches), their license, and the results the site cites, read from
  the links of website/public/index.html and ja/index.html;
- `adopters.json`: the organizations listed in ADOPTERS.md, each added by itself.

Run it after tools/parity/compatibility.py. Stdlib only.
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "parity"))
import compatibility  # noqa: E402  (tools/parity/compatibility.py)

sdk = compatibility.sdk
REPO = "https://github.com/hidane-dev/hidane"
BENCHMARK_REPO = "https://github.com/hidane-dev/benchmark-results"
BENCHMARK_INDEX = "https://raw.githubusercontent.com/hidane-dev/benchmark-results/main/index.json"
PAGES = ["website/public/index.html", "website/public/ja/index.html"]
ADOPTERS = "ADOPTERS.md"
ADOPTER_COLUMNS = ["Organization", "Project", "How it uses hidane", "Since", "Link"]
OUT = "website/public/data"
SCHEMA_VERSION = 1


class Stop(Exception):
    """A problem the person running the script has to fix."""


def source(rel):
    return (ROOT / rel).read_text(encoding="utf-8")


def blob(rel):
    return f"{REPO}/blob/main/{rel}"


def compatibility_data():
    report = compatibility.load(compatibility.OUTPUT)
    matrix = compatibility.load(sdk.OUTPUT)
    differences = compatibility.load(compatibility.DIFFERENCES)
    labels = {f["key"]: f["label"] for f in matrix["features"]}
    clients = []
    for c in matrix["clients"]:
        clients.append(dict(
            id=c["id"], client=c["client"], ecosystem=c["ecosystem"], package=c["package"],
            platform=c["platform"], transport=c["transport"], status=c["status"],
            version=c["version"], official_emulator=[r.removeprefix("v")
                                                     for r in c["official_emulator"]],
            exercised=[labels[f] for f in c["exercised"]],
            not_exercised=[labels[f] for f in c["not_exercised"]], gaps=c["gaps"],
            last_recorded=c["last_recorded"], tracking=c["tracking"],
            evidence=[dict(name=e["transcript"], result=e["result"],
                           url=blob(next(x["path"] for x in e["compared"]
                                         if x["path"].endswith("-hidane.json"))))
                      for e in c["evidence"]]))
    replay = report["replay"]
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_by": "tools/parity/site_data.py",
        "about": "What hidane's recorded results and tests show about its compatibility with the "
                 "official Cloud Firestore emulator, its SDKs and firebase-tools. Generated from "
                 "docs/compatibility.json, docs/sdk-certification.json and "
                 "docs/parity-exceptions.json; see their pages for how each value is made.",
        "sources": {
            "report": blob(compatibility.OUTPUT),
            "sdk_certification": blob(sdk.PAGE),
            "parity": blob("docs/parity.md"),
            "differences": blob("docs/parity-exceptions.md"),
        },
        "hidane": report["hidane"],
        "official_emulator": report["official_emulator"],
        "matrix": [dict(hidane=row["hidane"], firebase_tools=row["firebase_tools"],
                        official_emulator=row["official_emulator"], status=row["status"],
                        evidence=[e["description"] for e in row["evidence"]])
                   for row in report["matrix"]],
        "statuses": [dict(key=s["key"], label=s["label"]) for s in matrix["statuses"]],
        "clients": clients,
        "parity": dict(fixtures=replay["fixtures"], answers=replay["answers"],
                       levels=replay["levels"], transcripts=report["transcripts"]),
        "browser_suite": {key: report["browser"]["suite"][key]
                          for key in ("commit", "official", "hidane", "result")},
        "differences": dict(
            intentional=report["differences"]["intentional"],
            production=report["differences"]["production"],
            kinds=[dict(key=k["key"], meaning=k["meaning"]) for k in differences["kinds"]]),
    }


def cited_results():
    """The benchmark results the site's pages link to, the same on each."""
    cited = None
    for page in PAGES:
        found = sorted(set(re.findall(rf"{re.escape(BENCHMARK_REPO)}/blob/main/([\w/.-]+)",
                                      source(page))))
        if cited is not None and found != cited:
            raise Stop(f"{page} cites other benchmark results than {PAGES[0]}: {found}.")
        cited = found
    return cited or []


def benchmarks_data():
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_by": "tools/parity/site_data.py",
        "about": "Where hidane's canonical benchmark results are: hidane and the official "
                 "emulator measured on one dedicated machine, nightly and for each release. "
                 "The index lists every result with its kind, time, environment revision, "
                 "versions and stability; compare only within one environment revision, and "
                 "quote medians of stable runs.",
        "repository": BENCHMARK_REPO,
        "index": BENCHMARK_INDEX,
        "license": "CC BY 4.0",
        "attribution": "hidane canonical benchmark (hidane-dev/benchmark-results)",
        "methodology": blob("bench/README.md"),
        "cited": [dict(path=path, url=f"{BENCHMARK_REPO}/blob/main/{path}")
                  for path in cited_results()],
    }


def adopters():
    """The rows of the table under "## Adopters" in ADOPTERS.md."""
    section = re.search(r"^## Adopters$(.*?)(?=^## |\Z)", source(ADOPTERS), re.M | re.S)
    lines = [line for line in (section.group(1) if section else "").splitlines()
             if line.startswith("|")]
    if len(lines) < 2:
        raise Stop(f"{ADOPTERS}: no table under \"## Adopters\".")
    head = [c.strip() for c in lines[0].strip("|").split("|")]
    if head != ADOPTER_COLUMNS:
        raise Stop(f"{ADOPTERS}: the table's columns must be {' | '.join(ADOPTER_COLUMNS)}.")
    out = []
    for line in lines[2:]:
        cells = [c.strip() for c in line.strip("|").split("|")]
        if len(cells) != len(ADOPTER_COLUMNS):
            raise Stop(f"{ADOPTERS}: a row has {len(cells)} cells: {line}")
        organization, project, usage, since, link = cells
        url = re.fullmatch(r"\[[^\]]+\]\((https://[^)\s]+)\)|(https://\S+)", link)
        if not organization or not usage or not url:
            raise Stop(f"{ADOPTERS}: {organization or 'a row'} needs an organization, how it uses "
                       "hidane and an https link.")
        if not re.fullmatch(r"\d{4}-\d{2}", since):
            raise Stop(f"{ADOPTERS}: {organization}: Since is a month, YYYY-MM.")
        out.append(dict(organization=organization, project=project or None, usage=usage,
                        since=since, url=url.group(1) or url.group(2)))
    return out


def adopters_data():
    return {
        "schema_version": SCHEMA_VERSION,
        "generated_by": "tools/parity/site_data.py",
        "about": "Organizations that use hidane and listed themselves in ADOPTERS.md.",
        "source": blob(ADOPTERS),
        "adopters": adopters(),
    }


def outputs():
    return {f"{OUT}/{name}.json": json.dumps(data, indent=1, ensure_ascii=False) + "\n"
            for name, data in (("compatibility", compatibility_data()),
                               ("benchmarks", benchmarks_data()),
                               ("adopters", adopters_data()))}


def main():
    args = sys.argv[1:]
    if args not in ([], ["--check"]):
        raise Stop("usage: python3 tools/parity/site_data.py [--check]")
    stale = []
    for rel, text in outputs().items():
        path = ROOT / rel
        if path.is_file() and path.read_text(encoding="utf-8") == text:
            continue
        stale.append(rel)
        if not args:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
    if args and stale:
        raise Stop(f"stale: {', '.join(stale)}. Run python3 tools/parity/site_data.py and commit "
                   "the result.")
    for rel in stale:
        print(f"wrote {rel}")


if __name__ == "__main__":
    try:
        main()
    except (Stop, compatibility.Stop, sdk.Stop, sdk.corpus.Stop, sdk.table.Stop) as stop:
        sys.exit(f"tools/parity/site_data.py: {stop}")
