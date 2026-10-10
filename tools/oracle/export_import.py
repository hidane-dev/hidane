"""How does the official emulator export and import data? (#88, #31, #32)

Usage:
    python3 -I tools/oracle/export_import.py 127.0.0.1:8086 <cloud-firestore-emulator.jar> [java] \
        > crates/hidane/tests/fixtures/export_import.json

Against a freshly started official emulator, writes documents holding every kind of value and
exports them with `POST /emulator/v1/projects/{p}:export`; the exported files are recorded as
they are (base64), so hidane's export can be compared with them byte for byte and its import
can read them. A document of 70,000 bytes, whose record spans three 32 KiB blocks, is recorded
by length and CRC-32C only. Then it imports exports with `:import`, records the endpoints'
answers to bad requests and damaged files, and starts three more emulators of its own (the jar
and `java` given as arguments) to observe `--seed_from_export`: which databases are seeded,
when, and after `/reset` and a clear; which seed files stop the start; and that
`--export-on-exit` writes nothing.

No production export is at hand, so one is made from the official export the way a managed
export of Firestore in Native mode is laid out: a kind directory per collection with several
output files, the app `s~{project}`, and top-level properties indexed (field 14) unless they
cannot be (bytes, maps, strings over 1,500 bytes). The official emulator's import of it is
recorded with the files.

Exported files hold absolute paths only in error messages, recorded with the export directory
replaced by `{dir}`. `crates/hidane/tests/export_import.rs` replays the fixture.
"""

import base64
import http.client
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from urllib.parse import quote

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
JAR = sys.argv[2]
JAVA = sys.argv[3] if len(sys.argv) > 3 else "java"
PROJECT = "demo-export"


def request(host, method, path, body=None, raw=None, headers=None):
    conn = http.client.HTTPConnection(host, timeout=60)
    data = raw if raw is not None else (None if body is None else json.dumps(body))
    conn.request(method, path, body=data, headers={"Content-Type": "application/json", **(headers or {})})
    res = conn.getresponse()
    text = res.read().decode()
    return res.status, res.getheader("Content-Type"), text


def rest(host, method, path, body=None):
    status, _, text = request(host, method, "/v1/" + quote(path, safe="/:()"), body)
    return status, json.loads(text) if text.strip().startswith(("{", "[")) else text


def vector(*xs):
    return {"mapValue": {"fields": {"__type__": {"stringValue": "__vector__"},
                                    "value": {"arrayValue": {"values": [{"doubleValue": x} for x in xs]}}}}}


def ref(path, project=PROJECT, database="(default)"):
    return {"referenceValue": f"projects/{project}/databases/{database}/documents/{path}"}


def m(**fields):
    return {"mapValue": {"fields": fields} if fields else {}}


def arr(*values):
    return {"arrayValue": {"values": list(values)} if values else {}}


I = lambda n: {"integerValue": str(n)}
D = lambda x: {"doubleValue": x}
S = lambda s: {"stringValue": s}
T = lambda t: {"timestampValue": t}

TYPES = {
    "array": arr(I(1), S("two"), {"nullValue": None}, m(k=S("v")), m(), vector(3.0)),
    "bool_false": {"booleanValue": False},
    "bool_true": {"booleanValue": True},
    "bytes": {"bytesValue": "AAEC/w=="},
    "dotted.name": I(3),
    "double": D(1.5),
    "double_inf": D("Infinity"),
    "double_nan": D("NaN"),
    "double_neg_inf": D("-Infinity"),
    "double_neg_zero": D(-0.0),
    "empty_array": arr(),
    "empty_map": m(),
    "empty_string": S(""),
    "geo": {"geoPointValue": {"latitude": 35.5, "longitude": -139.25}},
    "int": I(42),
    "int_max": I(9223372036854775807),
    "int_min": I(-9223372036854775808),
    "int_neg": I(-7),
    "map": m(a=I(1), inner=m(z={"booleanValue": True}), nested_empty_array=arr(), nested_empty_map=m(),
             nested_vector=vector(1.0, 2.0)),
    "null": {"nullValue": None},
    "ref": ref("all/types"),
    "ref_db2": ref("c/x", database="db2"),
    "ref_deep": ref("c/a/sub/x"),
    "ref_numeric": ref("c/__id7__"),
    "ref_other_project": ref("c/x", project="elsewhere"),
    "string": S("héllo 😀"),
    "timestamp": T("2024-01-02T03:04:05.123456Z"),
    "timestamp_min": T("0001-01-01T00:00:00Z"),
    "timestamp_nanos": T("2024-01-01T00:00:00.123456789Z"),
    "timestamp_old": T("1960-01-01T00:00:00.500Z"),
    "vector": vector(1.0, 2.5),
    "with space": S("x"),
}

# In hidane's path order (numeric IDs first, then UTF-8), fields in UTF-8 order, so the
# official export (which keeps the order documents and fields were written in) can be compared
# with hidane's byte for byte.
DOCUMENTS = [
    ("(default)", "all/types", TYPES),
    ("(default)", "c/__id-3__", {"n": I(1)}),
    ("(default)", "c/__id7__", {"n": I(2)}),
    ("(default)", "c/a", {"n": I(3)}),
    ("(default)", "c/a/sub/x", {"n": I(4)}),
    ("(default)", "c/weird id ü", {"n": I(5)}),
    ("(default)", "empty/doc", {}),
    ("(default)", "long/doc", {"s": S("x" * 3000)}),
    ("(default)", "missing/m/sub/y", {"n": I(6)}),
    ("(default)", "refs/doc", {
        "array": arr(ref("c/a"), ref("c/b", project="elsewhere")),
        "array_of_maps": arr(m(r=ref("c/q", project="elsewhere"))),
        "map": m(deeper=m(r=ref("c/x", project="elsewhere", database="db2")), inner=ref("c/a", project="elsewhere")),
        "single": ref("c/z", project="elsewhere"),
    }),
    ("db2", "c/__id5__", {"n": I(7)}),
    ("db2", "c/x", {"n": I(8)}),
]
BIG = ("big/doc", 70_000)


def crc32c(data):
    crc = 0xFFFFFFFF
    for byte in data:
        crc ^= byte
        for _ in range(8):
            crc = (crc >> 1) ^ (0x82F63B78 if crc & 1 else 0)
    return crc ^ 0xFFFFFFFF


def write(host, project, database, path, fields):
    status, _ = rest(host, "PATCH", f"projects/{project}/databases/{database}/documents/{path}", {"fields": fields})
    assert status == 200, (path, status)


def export(host, project, database, directory, name):
    os.makedirs(directory, exist_ok=True)
    status, _, text = request(host, "POST", f"/emulator/v1/projects/{project}:export", {
        "database": f"projects/{project}/databases/{database}", "export_directory": directory, "export_name": name})
    assert status == 200, text
    return os.path.join(directory, name)


def files(root):
    out = {}
    for base, _, names in os.walk(root):
        for name in sorted(names):
            full = os.path.join(base, name)
            out[os.path.relpath(full, root)] = base64.b64encode(open(full, "rb").read()).decode()
    return dict(sorted(out.items()))


def all_documents(host, project, database="(default)"):
    status, rows = rest(host, "POST", f"projects/{project}/databases/{database}/documents:runQuery",
                        {"structuredQuery": {"from": [{"allDescendants": True}]}})
    assert status == 200, rows
    return [{"name": r["document"]["name"], "fields": r["document"].get("fields", {}),
             "createTimeIsUpdateTime": r["document"]["createTime"] == r["document"]["updateTime"]}
            for r in rows if "document" in r]


def get(host, project, database, path):
    status, body = rest(host, "GET", f"projects/{project}/databases/{database}/documents/{path}")
    return status, body


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Emulator:
    """An official emulator started by this script, stopped with SIGINT as firebase-tools does."""

    def __init__(self, *args):
        self.port = free_port()
        self.host = f"127.0.0.1:{self.port}"
        self.proc = subprocess.Popen(
            [JAVA, "-jar", JAR, "--host", "127.0.0.1", "--port", str(self.port), *args],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
            preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL))
        self.up = False
        for _ in range(120):
            if self.proc.poll() is not None:
                break
            try:
                request(self.host, "GET", "/")
                self.up = True
                break
            except OSError:
                time.sleep(0.25)

    def stop(self):
        if self.proc.poll() is None:
            self.proc.send_signal(signal.SIGINT)
        output = self.proc.communicate(timeout=30)[0]
        return self.proc.returncode, output


def startup_error(args):
    emulator = Emulator(*args)
    assert not emulator.up, args
    code, output = emulator.stop()
    # The exception's message, without the stack trace.
    lines = [l for l in output.splitlines() if l.startswith("com.google.cloud.datastore.core.exception.")]
    return code, lines[0].split(": ", 1)[1]


def main():
    work = os.path.realpath(tempfile.mkdtemp())
    try:
        fixture = run(work)
    finally:
        shutil.rmtree(work)
    json.dump(fixture, sys.stdout, indent=1, ensure_ascii=False)
    print()


def run(work):
    fixture = {"documents": [{"database": d, "path": p, "fields": f} for d, p, f in DOCUMENTS]}

    for database, path, fields in DOCUMENTS:
        write(HOST, PROJECT, database, path, fields)
    exports = {
        "default": export(HOST, PROJECT, "(default)", f"{work}/default", "firestore_export"),
        "db2": export(HOST, PROJECT, "db2", f"{work}/db2", "firestore_export"),
        "empty": export(HOST, "demo-empty", "(default)", f"{work}/empty", "firestore_export"),
    }
    fixture["exports"] = {key: files(path) for key, path in exports.items()}

    write(HOST, "demo-big", "(default)", BIG[0], {"s": S("y" * BIG[1])})
    big = export(HOST, "demo-big", "(default)", f"{work}/big", "firestore_export")
    output = open(f"{big}/all_namespaces/all_kinds/output-0", "rb").read()
    fixture["big"] = {"path": BIG[0], "length": BIG[1], "output_len": len(output), "output_crc32c": crc32c(output)}

    default_meta = f"{exports['default']}/firestore_export.overall_export_metadata"
    db2_meta = f"{exports['db2']}/firestore_export.overall_export_metadata"

    # :import writes the documents whose key names the target database, into the target
    # project; the export's project is not kept, references are not rewritten.
    imports = {}
    for project, database, meta in [("demo-imported", "(default)", default_meta), ("demo-imported", "db2", db2_meta),
                                    ("demo-mismatch", "(default)", db2_meta), ("demo-mismatch", "db2", default_meta)]:
        status, _, text = request(HOST, "POST", f"/emulator/v1/projects/{project}:import",
                                  {"database": f"projects/{project}/databases/{database}", "export_directory": meta})
        assert status == 200, text
        imports[f"{project}/{database}"] = all_documents(HOST, project, database)
    fixture["imported"] = imports

    # Importing over existing documents, and importing the same export twice.
    write(HOST, "demo-over", "(default)", "all/types", {"mine": I(1)})
    write(HOST, "demo-over", "(default)", "keep/doc", {"keep": I(2)})
    _, before = get(HOST, "demo-over", "(default)", "all/types")
    _, keep_before = get(HOST, "demo-over", "(default)", "keep/doc")
    body = {"database": "projects/demo-over/databases/(default)", "export_directory": default_meta}
    request(HOST, "POST", "/emulator/v1/projects/demo-over:import", body)
    _, after = get(HOST, "demo-over", "(default)", "all/types")
    request(HOST, "POST", "/emulator/v1/projects/demo-over:import", body)
    _, again = get(HOST, "demo-over", "(default)", "all/types")
    _, keep_after = get(HOST, "demo-over", "(default)", "keep/doc")
    fixture["import_over"] = {
        "fields_replaced": sorted(after["fields"]) == sorted(TYPES),
        "create_time_kept": after["createTime"] == before["createTime"],
        "update_time_changed": after["updateTime"] != before["updateTime"],
        "reimport_keeps_update_time": again["updateTime"] == after["updateTime"],
        "other_document_untouched": keep_after == keep_before,
    }

    production = f"{work}/production"
    production_shaped(exports["default"], production)
    status, _, text = request(HOST, "POST", "/emulator/v1/projects/demo-production:import", {
        "database": "projects/demo-production/databases/(default)",
        "export_directory": f"{production}/production.overall_export_metadata"})
    assert status == 200, text
    fixture["production"] = {"files": files(production), "imported": all_documents(HOST, "demo-production")}

    fixture["requests"] = endpoint_cases(work, default_meta)
    fixture["seeding"] = seeding(default_meta)
    fixture["startup"] = startup(work, default_meta)
    return fixture


def varint(value):
    out = bytearray()
    while value >= 0x80:
        out.append(value & 0x7F | 0x80)
        value >>= 7
    out.append(value)
    return bytes(out)


def parse_fields(data):
    """Protobuf fields as [(number, wire type, value)]; a group's value is its field list."""
    fields, stack, pos = [], [], 0

    def read_varint():
        nonlocal pos
        value = shift = 0
        while True:
            byte = data[pos]
            pos += 1
            value |= (byte & 0x7F) << shift
            shift += 7
            if byte < 0x80:
                return value

    current = fields
    while pos < len(data):
        tag = read_varint()
        number, wire = tag >> 3, tag & 7
        if wire == 0:
            current.append((number, 0, read_varint()))
        elif wire == 1:
            current.append((number, 1, data[pos:pos + 8]))
            pos += 8
        elif wire == 2:
            n = read_varint()
            current.append((number, 2, data[pos:pos + n]))
            pos += n
        elif wire == 3:
            group = []
            current.append((number, 3, group))
            stack.append(current)
            current = group
        elif wire == 4:
            current = stack.pop()
        elif wire == 5:
            current.append((number, 5, data[pos:pos + 4]))
            pos += 4
    return fields


def serialize(fields):
    out = bytearray()
    for number, wire, value in fields:
        if wire == 0:
            out += varint(number << 3) + varint(value)
        elif wire in (1, 5):
            out += varint(number << 3 | wire) + value
        elif wire == 2:
            out += varint(number << 3 | 2) + varint(len(value)) + value
        elif wire == 3:
            out += varint(number << 3 | 3) + serialize(value) + varint(number << 3 | 4)
    return bytes(out)


def log_records(data):
    records, pos, pending = [], 0, None
    while pos < len(data):
        if 32768 - pos % 32768 < 7:
            pos += 32768 - pos % 32768
            continue
        length, kind = int.from_bytes(data[pos + 4:pos + 6], "little"), data[pos + 6]
        fragment = data[pos + 7:pos + 7 + length]
        pos += 7 + length
        if kind == 1:
            records.append(fragment)
        elif kind == 2:
            pending = bytearray(fragment)
        elif kind == 3:
            pending += fragment
        elif kind == 4:
            records.append(bytes(pending + fragment))
    return records


def log_write(records):
    out, offset = bytearray(), 0
    for record in records:
        first = True
        while True:
            if 32768 - offset < 7:
                out += bytes(32768 - offset)
                offset = 0
            room = 32768 - offset - 7
            fragment, record = record[:room], record[room:]
            last = not record
            kind = 1 if first and last else 2 if first else 4 if last else 3
            crc = crc32c(bytes([kind]) + fragment)
            masked = (((crc >> 15) | (crc << 17)) + 0xA282EAD8) & 0xFFFFFFFF
            out += masked.to_bytes(4, "little") + len(fragment).to_bytes(2, "little") + bytes([kind]) + fragment
            offset += 7 + len(fragment)
            first = False
            if last:
                break
    return bytes(out)


def production_entity(data, top=True):
    """`s~` apps, and top-level properties indexed where Datastore allows it."""
    app = lambda v: b"s~" + v[4:] if v.startswith(b"dev~") else v
    out = []
    for number, wire, value in parse_fields(data):
        if number == 13 and wire == 2 and top:
            value = serialize([(n, w, app(v) if n == 13 else v) for n, w, v in parse_fields(value)])
        elif number in (14, 15) and wire == 2:
            prop = parse_fields(value)
            meaning = next((v for n, _, v in prop if n == 1), 0)
            rewritten = []
            for n, w, v in prop:
                if n == 5:
                    pv = []
                    for n3, w3, v3 in parse_fields(v):
                        if n3 == 12 and w3 == 3:
                            v3 = [(a, b, app(c) if a == 13 else c) for a, b, c in v3]
                        if n3 == 3 and meaning == 19:
                            v3 = production_entity(v3, top=False)
                        pv.append((n3, w3, v3))
                    v = serialize(pv)
                rewritten.append((n, w, v))
            long_string = any(n == 5 and any(n3 == 3 and len(v3) > 1500 for n3, _, v3 in parse_fields(v))
                              for n, _, v in prop)
            if top and meaning not in (14, 19) and not long_string:
                number = 14
            value = serialize(rewritten)
        out.append((number, wire, value))
    return serialize(out)


def production_shaped(source, target):
    """The export at `source` laid out as a managed export: `all_namespaces/kind_{c}/` per
    collection, each with two output files."""
    records = log_records(open(f"{source}/all_namespaces/all_kinds/output-0", "rb").read())
    kinds = {}
    for record in records:
        key = next(v for n, _, v in parse_fields(record) if n == 13)
        path = next(v for n, _, v in parse_fields(key) if n == 14)
        element = next(v for n, _, v in parse_fields(path) if n == 1)
        kind = next(v for n, _, v in element if n == 2).decode()
        kinds.setdefault(kind, []).append(production_entity(record))
    pointers = b""
    for kind, entities in sorted(kinds.items()):
        directory = f"all_namespaces/kind_{kind}"
        os.makedirs(f"{target}/{directory}")
        listing, total = b"", 0
        for i, part in enumerate([entities[: (len(entities) + 1) // 2], entities[(len(entities) + 1) // 2:]]):
            data = log_write(part)
            total += len(data)
            open(f"{target}/{directory}/output-{i}", "wb").write(data)
            listing += serialize([(2, 2, serialize([(1, 2, b""), (2, 2, f"output-{i}".encode())]))])
        metadata_path = f"{directory}/all_namespaces_kind_{kind}.export_metadata"
        metadata = serialize([(1, 2, serialize([(1, 2, b"production"), (2, 0, 1), (3, 0, 2)]))]) + listing
        open(f"{target}/{metadata_path}", "wb").write(metadata)
        pointers += serialize([(1, 2, serialize([(1, 2, serialize([(1, 0, 2), (3, 0, 3)])), (2, 2, metadata_path.encode()),
                                                  (3, 0, len(entities)), (4, 0, total)]))])
    open(f"{target}/production.overall_export_metadata", "wb").write(log_write([b"\x33", pointers]))


def endpoint_cases(work, default_meta):
    os.makedirs(f"{work}/out", exist_ok=True)
    open(f"{work}/out/file.txt", "w").close()
    damaged(work, default_meta)
    db = f"projects/{PROJECT}/databases/(default)"
    ok = "{dir}/out"
    E = f"/emulator/v1/projects/{PROJECT}:export"
    M = f"/emulator/v1/projects/{PROJECT}:import"
    meta = "{dir}/default/firestore_export/firestore_export.overall_export_metadata"
    cases = [
        ("export", "POST", E, {"database": db, "export_directory": ok, "export_name": "named"}),
        ("export again over the same name", "POST", E, {"database": db, "export_directory": ok, "export_name": "named"}),
        ("export with camelCase keys", "POST", E, {"database": db, "exportDirectory": ok, "exportName": "camel"}),
        ("export with null values", "POST", E, {"database": db, "export_directory": ok, "export_name": None}),
        ("export another project than the path's", "POST", E,
         {"database": "projects/demo-empty/databases/(default)", "export_directory": ok, "export_name": "other"}),
        ("export a database path", "POST", f"/emulator/v1/projects/{PROJECT}/databases/(default):export",
         {"database": db, "export_directory": ok, "export_name": "dbpath"}),
        ("export with an Authorization header", "POST", E,
         {"database": db, "export_directory": ok, "export_name": "authorized"}, None, {"Authorization": "Bearer owner"}),
        ("export with a bad Authorization header", "POST", E,
         {"database": db, "export_directory": ok, "export_name": "badauth"}, None, {"Authorization": "Bearer x.y"}),
        ("export into a missing directory", "POST", E, {"database": db, "export_directory": "{dir}/nosuch", "export_name": "n"}),
        ("export into a file", "POST", E, {"database": db, "export_directory": "{dir}/out/file.txt", "export_name": "n"}),
        ("export into a relative directory", "POST", E, {"database": db, "export_directory": "relative", "export_name": "n"}),
        ("export without a directory", "POST", E, {"database": db, "export_name": "n"}),
        ("export without a database", "POST", E, {"export_directory": ok, "export_name": "n"}),
        ("export a malformed database name", "POST", E, {"database": "garbage", "export_directory": ok, "export_name": "n"}),
        ("export a database name without a database id", "POST", E,
         {"database": f"projects/{PROJECT}/databases/", "export_directory": ok, "export_name": "n"}),
        ("export with a name holding a slash", "POST", E, {"database": db, "export_directory": ok, "export_name": "a/b"}),
        ("export with an unknown key", "POST", E, {"database": db, "export_directory": ok, "export_name": "n", "bogus": 1}),
        ("export with a number for the database", "POST", E, {"database": 1, "export_directory": ok, "export_name": "n"}),
        ("export a JSON array", "POST", E, None, "[]"),
        ("export a body that is not JSON", "POST", E, None, "not json"),
        ("export without a body", "POST", E, None, ""),
        ("export as text/plain", "POST", E, {"database": db, "export_directory": ok, "export_name": "plain"}, None,
         {"Content-Type": "text/plain"}),
        ("GET export", "GET", E, None, ""),
        ("PUT export", "PUT", E, {"database": db, "export_directory": ok, "export_name": "n"}),
        ("export under projects without a project", "POST", "/emulator/v1/projects:export",
         {"database": db, "export_directory": ok, "export_name": "n"}),
        ("export outside projects", "POST", "/emulator/v1/x:export", {"database": db, "export_directory": ok, "export_name": "n"}),
        ("export with a trailing slash", "POST", E + "/", {"database": db, "export_directory": ok, "export_name": "n"}),
        ("export with a longer verb", "POST", E + "x", {"database": db, "export_directory": ok, "export_name": "n"}),
        ("import", "POST", M, {"database": "projects/demo-imp/databases/(default)", "export_directory": meta}),
        ("import with camelCase keys", "POST", M, {"database": "projects/demo-imp2/databases/(default)", "exportDirectory": meta}),
        ("import an export directory", "POST", M,
         {"database": "projects/demo-imp/databases/(default)", "export_directory": "{dir}/default/firestore_export"}),
        ("import a missing file", "POST", M, {"database": "projects/demo-imp/databases/(default)", "export_directory": "{dir}/nosuch"}),
        ("import without a file", "POST", M, {"database": "projects/demo-imp/databases/(default)"}),
        ("import without a database", "POST", M, {"export_directory": meta}),
        ("import with an export name", "POST", M,
         {"database": "projects/demo-imp/databases/(default)", "export_directory": meta, "export_name": "n"}),
        ("import a file of another version", "POST", M,
         {"database": "projects/demo-bad/databases/(default)", "export_directory": "{dir}/bad/version.overall_export_metadata"}),
        ("import a file that is not a log", "POST", M,
         {"database": "projects/demo-bad/databases/(default)", "export_directory": "{dir}/bad/garbage.overall_export_metadata"}),
        ("import without its export_metadata", "POST", M,
         {"database": "projects/demo-bad/databases/(default)", "export_directory": "{dir}/bad/nometa/firestore_export.overall_export_metadata"}),
        ("import without its output", "POST", M,
         {"database": "projects/demo-bad/databases/(default)", "export_directory": "{dir}/bad/nooutput/firestore_export.overall_export_metadata"}),
        ("import an output with a bad checksum", "POST", M,
         {"database": "projects/demo-bad/databases/(default)", "export_directory": "{dir}/bad/crc/firestore_export.overall_export_metadata"}),
        ("import a truncated output", "POST", M,
         {"database": "projects/demo-bad/databases/(default)", "export_directory": "{dir}/bad/trunc/firestore_export.overall_export_metadata"}),
        ("GET import", "GET", M, None, ""),
    ]
    recorded = []
    for name, method, path, body, *rest in cases:
        raw = rest[0] if rest else None
        headers = rest[1] if len(rest) > 1 else None
        fill = lambda text: text.replace("{dir}", work)
        sent = None if body is None else json.loads(fill(json.dumps(body)))
        status, content_type, text = request(HOST, method, path, sent, raw if raw is not None else None, headers)
        written = sorted(os.listdir(f"{work}/out"))
        recorded.append({
            "name": name, "method": method, "path": path, "body": body, "raw": raw, "headers": headers,
            "status": status, "content_type": content_type,
            # Error messages escape "/" as protobuf-java's JSON printer does.
            "response": text.replace(work.replace("/", "\\/"), "{dir}").replace(work, "{dir}"),
            "export_directory_lists": written,
        })
        if "demo-imp" in json.dumps(body or {}) and status == 200:
            recorded[-1]["imported"] = len(all_documents(HOST, sent["database"].split("/")[1]))
    return recorded


def damaged(work, default_meta):
    """Copies of the (default) export with one part missing or broken, and two bad metadata files."""
    os.makedirs(f"{work}/bad", exist_ok=True)
    source = os.path.dirname(default_meta)
    for variant in ["nometa", "nooutput", "crc", "trunc"]:
        shutil.copytree(source, f"{work}/bad/{variant}")
    kinds = "all_namespaces/all_kinds"
    os.remove(f"{work}/bad/nometa/{kinds}/all_namespaces_all_kinds.export_metadata")
    os.remove(f"{work}/bad/nooutput/{kinds}/output-0")
    path = f"{work}/bad/crc/{kinds}/output-0"
    data = bytearray(open(path, "rb").read())
    data[20] ^= 0xFF
    open(path, "wb").write(data)
    path = f"{work}/bad/trunc/{kinds}/output-0"
    data = open(path, "rb").read()
    open(path, "wb").write(data[: len(data) // 2])
    # A log whose first record is 0x34 instead of 0x33.
    record = b"\x34"
    crc = crc32c(b"\x01" + record)
    masked = (((crc >> 15) | (crc << 17)) + 0xA282EAD8) & 0xFFFFFFFF
    header = masked.to_bytes(4, "little") + len(record).to_bytes(2, "little") + b"\x01"
    open(f"{work}/bad/version.overall_export_metadata", "wb").write(header + record)
    open(f"{work}/bad/garbage.overall_export_metadata", "wb").write(b"garbage")


def seeding(default_meta):
    """Which databases a seed fills, when, and after a clear and a reset."""
    emulator = Emulator("--seed_from_export", default_meta)
    assert emulator.up
    h = emulator.host
    steps = []

    def step(what, method, path, body=None):
        status, _, text = request(h, method, quote(path, safe="/:()"), body)
        entry = {"what": what, "method": method, "path": path, "status": status}
        if path.startswith("/v1/") and status == 200 and method == "GET":
            doc = json.loads(text)
            entry["fields"] = sorted(doc.get("fields", {}))
            entry["createTimeIsUpdateTime"] = doc["createTime"] == doc["updateTime"]
        steps.append(entry)

    doc = lambda p, d="(default)", path="c/a": f"/v1/projects/{p}/databases/{d}/documents/{path}"
    step("a project's (default) database is seeded on first access", "GET", doc("p1"))
    step("with every document of the export", "GET", doc("p1", path="all/types"))
    step("a document only a missing parent held", "GET", doc("p1", path="missing/m/sub/y"))
    step("another database of the project is not seeded from a (default) export", "GET", doc("p1", "db2"))
    step("every project is seeded", "GET", doc("p2"))
    step("clear p1", "DELETE", "/emulator/v1/projects/p1/databases/(default)/documents")
    step("a cleared database is not seeded again", "GET", doc("p1"))
    step("write to p2", "PATCH", doc("p2", path="new/doc"), {"fields": {"n": I(1)}})
    step("reset", "POST", "/reset")
    step("after a reset, a database is seeded again", "GET", doc("p2"))
    step("and what was written is gone", "GET", doc("p2", path="new/doc"))
    step("a cleared database is seeded again after a reset", "GET", doc("p1"))
    emulator.stop()
    return steps


def startup(work, default_meta):
    out = []
    for name, args in [
        ("a missing seed file", ["--seed_from_export", f"{work}/nosuch.overall_export_metadata"]),
        ("a seed file of another version", ["--seed_from_export", f"{work}/bad/version.overall_export_metadata"]),
        ("a seed export without its output", ["--seed_from_export", f"{work}/bad/nooutput/firestore_export.overall_export_metadata"]),
        ("a missing --import-data file", ["--import-data", f"{work}/nosuch.overall_export_metadata"]),
    ]:
        code, message = startup_error(args)
        out.append({"name": name, "args": [a.replace(work, "{dir}") for a in args], "exit": code,
                    "message": message.replace(work, "{dir}")})

    emulator = Emulator("--import-data", default_meta)
    status, _, _ = request(emulator.host, "GET", "/v1/projects/p/databases/(default)/documents/c/a")
    emulator.stop()
    out.append({"name": "--import-data seeds like --seed_from_export", "status": status})

    target = f"{work}/on-exit"
    os.makedirs(target)
    emulator = Emulator("--export-on-exit", target, "--export-name", "named")
    request(emulator.host, "PATCH", "/v1/projects/p/databases/(default)/documents/c/a", {"fields": {"n": I(1)}})
    code, _ = emulator.stop()
    out.append({"name": "--export-on-exit writes nothing", "exit": code, "files": sorted(os.listdir(target))})
    return out


if __name__ == "__main__":
    main()
