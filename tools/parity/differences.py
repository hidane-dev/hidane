"""Checks the official emulator difference database and writes its tables.

Usage (from the repository root):
    python3 tools/parity/differences.py           # writes the tables of docs/parity-exceptions.md
    python3 tools/parity/differences.py --check   # fails if they are stale or the database is invalid

docs/parity-exceptions.json is the database: every place where the official Cloud Firestore
emulator, production Firestore and hidane do not all behave the same, one entry each, with what
the official emulator does, what production Firestore does where the entry knows it, what hidane
does, why, the issues, documents and files it refers to, and its kind:

- `bug-for-bug`: hidane does what the official emulator does, where production differs;
- `more-correct`: hidane departs from the official emulator to do what production Firestore or the
  documented contract does, or to fix what is a bug whatever production does;
- `equivalent`: hidane departs in a detail clients do not observe or cannot rely on (wording,
  transport, timing, order, opaque formats), with the same outcome;
- `limitation`: hidane departs because it does less, or bounds what the official emulator does
  not.

The database is edited by hand; this script validates it against
docs/parity-exceptions.schema.json, checks that every issue, document and file an entry's text
mentions is among its `references` and that each file exists, and writes the two tables of
docs/parity-exceptions.md (between `<!-- differences:<table>:start -->` and
`<!-- differences:<table>:end -->`) from it. tools/parity/table.py counts the rows of the first
table. Stdlib only.
"""

import json
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "tools" / "oracle"))
import corpus  # noqa: E402  (its JSON Schema validator)

DATABASE = "docs/parity-exceptions.json"
SCHEMA = "docs/parity-exceptions.schema.json"
PAGE = "docs/parity-exceptions.md"
INTENTIONAL, PRODUCTION = "intentional", "production"
KINDS = {
    "bug-for-bug": "bug-for-bug",
    "more-correct": "more correct",
    "equivalent": "equivalent",
    "limitation": "limitation",
}
HIDANE = "hidane-dev/hidane"
FIREBASE_TOOLS = "firebase/firebase-tools"
# Directories of the repository a path in an entry's text can start with.
REPO_DIRS = ("bench/", "crates/", "docs/", "results/", "spikes/", "tests/", "tools/")


class Stop(Exception):
    """A problem the person running the script has to fix."""


def source(rel):
    return (ROOT / rel).read_text(encoding="utf-8")


def load(rel):
    with open(ROOT / rel, encoding="utf-8") as f:
        return json.load(f)


def repo_path(path):
    """A path an entry's text gives, as a path in the repository: `tests/fixtures/…` is
    hidane's (crates/hidane/tests/fixtures/…), a page linked from docs/ is under docs/."""
    path = path.split("#", 1)[0].rstrip("/")
    if path.startswith("tests/"):
        return f"crates/hidane/{path}"
    if path.startswith(REPO_DIRS):
        return path
    return f"docs/{path}"


def mentioned(text):
    """The references an entry's text makes: issues (`hidane-dev/hidane#86`,
    `firebase/firebase-tools#11241`), web pages, and the repository's files and pages."""
    found = []

    def add(ref):
        if ref not in found:
            found.append(ref)

    for number, url in re.findall(r"\[#(\d+)\]\((https?://[^)\s]+)\)", text):
        match = re.fullmatch(r"https://github\.com/([\w.-]+/[\w.-]+)/issues/(\d+)", url)
        add(f"{match.group(1)}#{match.group(2)}" if match else url)
    rest = re.sub(r"\[#\d+\]\([^)]*\)", "", text)
    for url in re.findall(r"\]\((https?://[^)\s]+)\)", rest):
        add(url)
    for number in re.findall(r"(?<![\w/\[#])#(\d+)\b", rest):
        add(f"{HIDANE}#{number}")
    for link in re.findall(r"\]\((?!https?://)([^)\s]+)\)", rest):
        add(repo_path(link))
    for number in re.findall(r"\bADR (\d{4})\b", rest):
        adr = sorted((ROOT / "docs" / "adr").glob(f"{number}-*.md"))
        if len(adr) != 1:
            raise Stop(f"ADR {number}: no single docs/adr/{number}-*.md.")
        add(adr[0].relative_to(ROOT).as_posix())
    for code in re.findall(r"`([^`\s]+)`", rest):
        if code.startswith(REPO_DIRS) and re.search(r"\.\w+$|/$", code):
            add(repo_path(code))
    return found


def check_reference(entry, ref):
    if re.fullmatch(r"[\w.-]+/[\w.-]+#\d+", ref) or re.fullmatch(r"https?://\S+", ref):
        return
    if any(c in ref for c in "*?["):
        if not list(ROOT.glob(ref)):
            raise Stop(f"{entry['id']}: no file matches its reference {ref}.")
    elif not (ROOT / ref).exists():
        raise Stop(f"{entry['id']}: its reference {ref} does not exist.")


def checked(data):
    schema = load(SCHEMA)
    found = corpus.problems(data, schema, schema)
    if found:
        raise Stop(f"{DATABASE} does not fit {SCHEMA}:\n  " + "\n  ".join(found[:20]))
    entries = data["differences"]
    ids = [entry["id"] for entry in entries]
    expected = [f"D{i:03d}" for i in range(1, len(entries) + 1)]
    if sorted(ids) != expected:
        raise Stop(f"{DATABASE}: the IDs must be D001 to D{len(entries):03d}, each once; a new "
                   f"entry takes the next one.")
    for entry in entries:
        production = entry["table"] == PRODUCTION
        if production != (entry["topic"] is not None):
            raise Stop(f"{entry['id']}: `topic` is the production table's Area column; set it for "
                       "an entry of that table only.")
        if production and entry["firestore"] is None:
            raise Stop(f"{entry['id']}: an entry of the production table says what production "
                       "Firestore does (`firestore`).")
        text = " ".join(entry[key] or "" for key in ("topic", "official", "firestore", "hidane",
                                                     "reason"))
        if "|" in text.replace("\\|", ""):
            raise Stop(f"{entry['id']}: escape `|` as `\\|`; it ends a table cell.")
        missing = [ref for ref in mentioned(text) if ref not in entry["references"]]
        if missing:
            raise Stop(f"{entry['id']}: add {', '.join(missing)} to its references (its text "
                       "mentions them).")
        for ref in entry["references"]:
            check_reference(entry, ref)
    return entries


def kind_cell(entry):
    return f"{KINDS[entry['kind']]} ({entry['id']})"


def tables(entries):
    """The two tables of docs/parity-exceptions.md, in the database's order."""
    out = {}
    for table, head, row in (
            (INTENTIONAL, "| Official emulator behaviour | hidane | Why | Kind |\n|---|---|---|---|",
             lambda e: f"| {e['official']} | {e['hidane']} | {e['reason']} | {kind_cell(e)} |"),
            (PRODUCTION, "| Area | Official emulator | Production Firestore | Decision | Kind |\n"
                         "|---|---|---|---|---|",
             lambda e: f"| {e['topic']} | {e['official']} | {e['firestore']} | {e['reason']} | "
                       f"{kind_cell(e)} |")):
        chosen = [e for e in entries if e["table"] == table]
        kinds = Counter(e["kind"] for e in chosen)
        summary = ", ".join(f"{kinds[key]} {label}" for key, label in KINDS.items() if kinds[key])
        start, end = markers(table)
        out[table] = "\n".join(
            [start, "<!-- Generated by tools/parity/differences.py from "
                    "docs/parity-exceptions.json; edit the database, not this. -->", "",
             f"{len(chosen)} entries: {summary}.", "", head] + [row(e) for e in chosen] + ["",
                                                                                          end])
    return out


def markers(table):
    return f"<!-- differences:{table}:start -->", f"<!-- differences:{table}:end -->"


def page(entries):
    text = source(PAGE)
    for table, block in tables(entries).items():
        start, end = markers(table)
        if text.count(start) != 1 or text.count(end) != 1 or text.index(start) > text.index(end):
            raise Stop(f"{PAGE}: expected one {start} before one {end}.")
        text = text[:text.index(start)] + block + text[text.index(end) + len(end):]
    return text


def main():
    args = sys.argv[1:]
    if args not in ([], ["--check"]):
        raise Stop("usage: python3 tools/parity/differences.py [--check]")
    text = page(checked(load(DATABASE)))
    if source(PAGE) == text:
        return
    if args:
        raise Stop(f"stale: {PAGE}. Run python3 tools/parity/differences.py and commit the "
                   "result.")
    (ROOT / PAGE).write_text(text, encoding="utf-8")
    print(f"wrote {PAGE}")


if __name__ == "__main__":
    try:
        main()
    except (Stop, corpus.Stop) as stop:
        sys.exit(f"tools/parity/differences.py: {stop}")
