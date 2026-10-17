#!/usr/bin/env python3
"""How does the official emulator in Datastore mode export and import? (#81)

Usage (from the repository root):
    python3 -I tools/oracle/datastore_export.py <cloud-firestore-emulator.jar> [java] [port] \\
        > crates/hidane/tests/fixtures/datastore_export.json

The script starts the jar three times on `port` (default 8220), from a temporary directory
(`{dir}` in the fixture) and under an English locale, with `--database-mode datastore-mode`:

- `export`: writes `DATASET` (a value of every type, meanings, excluded values, entity values,
  keys, namespaces, ancestors), then `POST /emulator/v1/projects/{p}:export` into `{dir}/out`,
  named and unnamed, and the requests it refuses. Each export is recorded as its files: the
  `.overall_export_metadata` file and each `.export_metadata` file byte for byte except the
  export's name and times, read with the same reader as hidane's tests (`fields`), and the
  records of each `output-*` file, base64, sorted: the emulator writes them in an order of its
  own, not the keys'.
- `import at startup`: `--import-data` names the named export of the first run, whose files the
  fixture keeps (`files`): its records sorted and its times fixed, as the emulator writes its
  records in an order that changes from run to run; then each entity is read back.
- `import`: `POST …:import` with that export, then each entity is read back; and the requests it
  refuses.

Server times, versions and cursors are masked as in `datastore_queries.json`.
`crates/hidane/tests/datastore.rs` replays it. Stdlib only.
"""

import base64
import http.client
import json
import os
import struct
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import datastore_queries as base  # noqa: E402

JAR = sys.argv[1]
JAVA = sys.argv[2] if len(sys.argv) > 2 else "java"
PORT = int(sys.argv[3]) if len(sys.argv) > 3 else 8220
base.HOST = f"127.0.0.1:{PORT}"
base.PROJECT = "demo-export"
P = base.P
key, I, S, ENT, ARR = base.key, base.I, base.S, base.ENT, base.ARR

DATASET = [
    (key(("T", "every")), {
        "i": I(-5), "d": {"doubleValue": 2.5}, "z": {"doubleValue": -0.0}, "b": {"booleanValue": True},
        "s": S("é"), "n": {"nullValue": None}, "t": {"timestampValue": "2020-01-02T03:04:05.678901Z"},
        "blob": {"blobValue": "AAE="}, "g": {"geoPointValue": {"latitude": 1.5, "longitude": -2.5}},
        "k": {"keyValue": key(("A", "x"), ("B", 7), ns="ns2")}, "a": ARR(I(1), S("z")), "ea": ARR(),
        "e": {"entityValue": {"key": key(("E", "k")), "properties": {
            "x": I(1), "y": {"stringValue": "n", "excludeFromIndexes": True}}}},
        "ek": ENT(x=I(2)),
        "ex": {"stringValue": "excluded", "excludeFromIndexes": True},
        "exa": ARR({"integerValue": "1", "excludeFromIndexes": True}, I(2)),
        "bx": {"blobValue": "AAEC", "excludeFromIndexes": True},
        "text": {"stringValue": "t", "meaning": 15, "excludeFromIndexes": True},
        "bytes": {"blobValue": "AQ==", "meaning": 16},
        "zlib": {"blobValue": "AQ==", "meaning": 22, "excludeFromIndexes": True},
        "m1": {"stringValue": "s", "meaning": 1},
        "nan": {"doubleValue": "NaN"}, "inf": {"doubleValue": "-Infinity"},
    }),
    (key(("T", 9), ("C", "c")), {}),
    (key(("T", 4503599627370497)), {"big": I(9223372036854775807)}),
    (key(("T", "x"), ns="ns1"), {"v": I(1)}),
    (key(("U", "u")), {"v": S("u")}),
]


def case(name, verb, body, path=None, defines=None):
    c = {"name": name, "method": "POST", "path": path or f"/v1/projects/{P}:{verb}", "headers": base.JSON,
         "body": body, "compare": "exact"}
    if defines:
        c["defines"] = defines
    return c


def emulator(name, verb, body, project=P):
    return case(name, verb, body, path=f"/emulator/v1/projects/{project}:{verb}")


def reads():
    """Each entity of DATASET, read back, and the kinds."""
    return [
        case("lookup every entity", "lookup", {"keys": [k for k, _ in DATASET]}),
        case("kind T", "runQuery", {"query": {"kind": [{"name": "T"}]}}),
        case("kind T in ns1", "runQuery", {"query": {"kind": [{"name": "T"}]}, "partitionId": {"namespaceId": "ns1"}}),
        case("kinds", "runQuery", {"query": {"kind": [{"name": "__kind__"}]}}),
        case("namespaces", "runQuery", {"query": {"kind": [{"name": "__namespace__"}]}}),
        case("an excluded value cannot be found", "runQuery", {"query": {"kind": [{"name": "T"}], "filter": {
            "propertyFilter": {"property": {"name": "ex"}, "op": "EQUAL", "value": S("excluded")}}}}),
        case("an indexed value can", "runQuery", {"query": {"kind": [{"name": "T"}], "filter": {
            "propertyFilter": {"property": {"name": "s"}, "op": "EQUAL", "value": S("é")}}}}),
    ]


DB = f"projects/{P}/databases/"


def export_cases():
    out = [case("seed", "commit", {"mode": "NON_TRANSACTIONAL", "mutations": [
        {"upsert": {"key": k, "properties": props}} for k, props in DATASET]})]
    out += [
        emulator("export, named", "export", {"database": DB, "export_directory": "{dir}/out", "export_name": "named"}),
        emulator("export, unnamed", "export", {"database": DB, "export_directory": "{dir}/out"}),
        emulator("export (default)", "export", {"database": f"projects/{P}/databases/(default)", "export_directory": "{dir}/out"}),
        emulator("export another database", "export", {"database": f"projects/{P}/databases/db2", "export_directory": "{dir}/out", "export_name": "db2"}),
        emulator("export, a project only", "export", {"database": f"projects/{P}", "export_directory": "{dir}/out"}),
        emulator("export, no database", "export", {"export_directory": "{dir}/out"}),
        emulator("export, no directory", "export", {"database": DB}),
        emulator("export, a directory that does not exist", "export", {"database": DB, "export_directory": "{dir}/nowhere"}),
        emulator("export, a file as the directory", "export", {"database": DB, "export_directory": "{dir}/out/named/named.overall_export_metadata"}),
        emulator("export, an unknown field", "export", {"database": DB, "export_directory": "{dir}/out", "x": 1}),
        emulator("export from another project's path", "export", {"database": DB, "export_directory": "{dir}/out", "export_name": "other"}, project="elsewhere"),
    ]
    return out


IMPORTED = "{dir}/files/named/named.overall_export_metadata"


def import_cases():
    return [
        emulator("import (default)", "import", {"database": f"projects/{P}/databases/(default)", "export_directory": IMPORTED}),
        emulator("import, a directory", "import", {"database": DB, "export_directory": "{dir}/files/named"}),
        emulator("import, a file that does not exist", "import", {"database": DB, "export_directory": "{dir}/files/none.overall_export_metadata"}),
        emulator("import, no directory", "import", {"database": DB}),
        emulator("import", "import", {"database": DB, "export_directory": IMPORTED}),
        *reads(),
        emulator("import again", "import", {"database": DB, "export_directory": IMPORTED}),
        case("lookup every entity after a second import", "lookup", {"keys": [k for k, _ in DATASET]}),
    ]


def records(data):
    """The records of a LevelDB log file (32 KiB blocks of fragments)."""
    out, partial, i = [], b"", 0
    while i + 7 <= len(data):
        left = 32768 - i % 32768
        if left < 7:
            i += left
            continue
        _, length, kind = struct.unpack("<IHB", data[i:i + 7])
        fragment = data[i + 7:i + 7 + length]
        i += 7 + length
        if kind == 1:
            out.append(fragment)
        elif kind == 2:
            partial = fragment
        elif kind == 3:
            partial += fragment
        elif kind == 4:
            out.append(partial + fragment)
            partial = b""
        elif kind == 0:
            break
    return out


def crc32c(data):
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
    return crc ^ 0xFFFFFFFF


def log(records):
    """A LevelDB log file of `records`, in 32 KiB blocks, as an export's files are."""
    out = bytearray()
    for record in records:
        first = True
        while True:
            left = 32768 - len(out) % 32768
            if left < 7:
                out += bytes(left)
                left = 32768
            chunk, record = record[:left - 7], record[left - 7:]
            kind = (1 if first else 4) if not record else (2 if first else 3)
            crc = crc32c(bytes([kind]) + chunk)
            masked = ((crc >> 15) | (crc << 17)) + 0xA282EAD8 & 0xFFFFFFFF
            out += struct.pack("<IHB", masked, len(chunk), kind) + chunk
            first = False
            if not record:
                break
    return bytes(out)


def varint(n):
    out = bytearray()
    while n >= 0x80:
        out.append(n & 0x7F | 0x80)
        n >>= 7
    out.append(n)
    return bytes(out)


def canonical(export):
    """The named export, made the same from run to run: its records sorted, its times fixed
    (2020-01-01). It holds the official emulator's records; the imports read these files."""
    name = os.path.basename(export)
    kind = os.path.join(export, "all_namespaces", "all_kinds")
    output = os.path.join(kind, "output-0")
    Path(output).write_bytes(log(sorted(records(Path(output).read_bytes()))))
    at = varint(1577836800000000)
    named = b"\n" + varint(len(name)) + name.encode() + b"\x10" + at + b"\x18" + at
    outputs = b"\n\x00\x12\x08output-0"
    metadata = b"\n" + varint(len(named)) + named + b"\x12" + varint(len(outputs)) + outputs
    Path(kind, "all_namespaces_all_kinds.export_metadata").write_bytes(metadata)


def tree(root, name):
    """An export as recorded: its files, the records of its output files sorted."""
    files = {}
    for dirpath, _, names in os.walk(root):
        for f in sorted(names):
            path = os.path.join(dirpath, f)
            rel = os.path.relpath(path, root).replace(name, "<name>")
            data = Path(path).read_bytes()
            if f.startswith("output-"):
                files[rel] = {"records": sorted(base64.b64encode(r).decode() for r in records(data))}
            elif f.endswith(".overall_export_metadata"):
                files[rel] = {"bytes": base64.b64encode(data).decode()}
            else:
                files[rel] = {"size": len(data)}
    return dict(sorted(files.items()))


def start(work, flags):
    proc = subprocess.Popen([JAVA, "-Duser.language=en", "-Duser.country=US", "-jar", JAR, "--host", "127.0.0.1",
                             "--port", str(PORT), "--database-mode", "datastore-mode", *flags],
                            cwd=work, stdout=open(os.path.join(work, "jar.out"), "wb"), stderr=subprocess.STDOUT)
    for _ in range(240):
        try:
            conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
            conn.request("GET", "/")
            if conn.getresponse().status == 200:
                return proc
        except OSError:
            time.sleep(0.25)
    proc.kill()
    printed = Path(work, "jar.out").read_text(errors="replace").splitlines()
    shown = [line for line in printed if not line.startswith(("\tat ", "WARNING: "))][-5:]
    raise SystemExit("the emulator did not start: " + " | ".join(shown).replace(work, "{dir}"))


def stop(proc):
    try:
        conn = http.client.HTTPConnection("127.0.0.1", PORT, timeout=5)
        conn.request("POST", "/shutdown")
        conn.getresponse().read()
    except OSError:
        pass
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()


def replay(cases, work):
    out = []
    for c in cases:
        text = json.dumps(c["body"]).replace("{dir}", work).replace("{project}", base.PROJECT)
        status, ctype, answer = base.call(c["method"], c["path"].replace("{project}", base.PROJECT), text)
        out.append(dict(c, outcome={"status": status, "contentType": ctype,
                                    "body": base.masked(answer.replace(work, "{dir}"))}))
    return out


def run(work, name, flags, cases, after=None):
    here = os.path.join(work, name.replace(" ", "-"))
    os.makedirs(os.path.join(here, "out"))
    if after:
        after(here)
    proc = start(here, [f.replace("{dir}", here) for f in flags])
    try:
        recorded = replay(cases, here)
    finally:
        stop(proc)
    return here, {"name": name, "flags": flags, "cases": recorded}


def main():
    with tempfile.TemporaryDirectory() as work:
        work = os.path.realpath(work)
        exported, first = run(work, "export", [], export_cases())
        out = os.path.join(exported, "out")
        exports = {}
        for entry in sorted(os.listdir(out)):
            shown = "datastore_export_<seconds>" if entry.startswith("datastore_export_") else entry
            exports[shown] = tree(os.path.join(out, entry), entry)
        first["exports"] = exports
        named = os.path.join(out, "named")
        canonical(named)
        files = {}
        for dirpath, _, names in os.walk(named):
            for f in names:
                path = os.path.join(dirpath, f)
                files[os.path.relpath(path, out)] = base64.b64encode(Path(path).read_bytes()).decode()

        def write_files(here):
            for rel, data in files.items():
                path = os.path.join(here, "files", rel)
                os.makedirs(os.path.dirname(path), exist_ok=True)
                Path(path).write_bytes(base64.b64decode(data))

        _, startup = run(work, "import at startup", ["--import-data", IMPORTED], reads(), write_files)
        _, endpoint = run(work, "import", [], import_cases(), write_files)
    json.dump({
        "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
        "generated_by": "tools/oracle/datastore_export.py",
        "mode": "datastore-mode",
        "files": dict(sorted(files.items())),
        "runs": [first, startup, endpoint],
    }, sys.stdout, ensure_ascii=False, indent=1)
    print()


if __name__ == "__main__":
    main()
