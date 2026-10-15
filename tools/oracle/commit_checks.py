#!/usr/bin/env python3
"""Which problem of a commit does the official emulator report first (#202)? What does it do
with a write to a document of another database (#203)?

Usage (from the repository root, `grpcurl` on PATH), against a freshly started jar:
    python3 -I tools/oracle/commit_checks.py 127.0.0.1:8086 EXPORT_DIR > crates/hidane/tests/fixtures/commit_checks.json
    python3 -I tools/oracle/commit_checks.py 127.0.0.1:8087 EXPORT_DIR enterprise \
        > crates/hidane/tests/fixtures/commit_checks_enterprise.json

EXPORT_DIR is an empty directory the jar writes an export to. The second runs the same cases
against a jar started with `--database-edition enterprise`, which checks values otherwise: no
size limit on a value or a document, arrays in arrays, 20 levels of maps and arrays, and a
problem inside a map reported as at the top.

`order`: commits with problems, each a kind of invalid write. Every ordered pair of the main
kinds (KINDS) runs as two writes (the first kind writes document `a`, the second document `b`),
every pair of the parts that fit in one update write (PARTS: a name, fields, a mask, transforms,
a precondition) runs as that one write, more parts (MORE_PARTS) run against one kind of each
stage (PROBES), and single writes (ALONE, SHAPES) show the order inside a field, a path or a
transform, and which kind of write may carry what. Each commit goes over REST `:commit`, gRPC
`Commit`, REST `:batchWrite` and the Write stream (a new stream each), in a project of its own
per transport (`projects/<project>-<transport>`), and the first error each answers is recorded.
Two sets of fields in one write go over REST only: grpcurl does not keep the order of a map's
entries.

What the recording shows, for `:commit` and gRPC `Commit` alike: first the verifies of the
whole commit (a document verified twice, verified and written, written and verified), then the
names of every write in order, then the sequences of writes to one document
(`same_document_writes.py`), then the values of every write in order, then each write is
applied in order (its precondition, then the document's size). The names of a write: an empty
write; a mask or transforms on a delete, a verify or a standalone transform; the document name;
a verify's precondition; then an update's fields in order, depth first (reserved names, a
`__type__` that is not a string, a vector's shape), its mask paths (parsing, reserved names,
`__name__` inside a longer path), then each transform's path, operand names and operand type.
The values of a write: an update's or a transform's key (IDs of at most 1500 bytes), then an
update's fields in order (an empty or long name, then the value: an array's elements in order,
nested arrays, a vector's NaN, long strings; anything wrong inside a map is "an invalid nested
entity"). The Write stream checks no verify: a verify there is only its precondition, applied in
order. `:batchWrite` refuses a document written twice before anything else, then checks the
names and then the values of every write; a precondition or a size fails its own write only. An
array union stores an operand holding an array or a vector with NaN as it is.

`other_database`: writes to a document of another database (and of another project) over each
transport, then where the documents ended up: reads in both databases, a batchGet in each that
names them, and an export of the request's database imported into a database of the other name.
Every transport acknowledges them; the documents stay in the request's database under the other
name, where only that batchGet and the export find them.

A string recorded as `{repeat:<c>:<n>}` (inside a name, a value or a message) stands for `<c>`
repeated `<n>` times, so the fixture stays small; the test expands it the same way. Each case
is written on one line. Stdlib only.
"""

import json
import re
import subprocess
import sys
import urllib.error
import urllib.request

HOST = sys.argv[1]
EXPORT_DIR = sys.argv[2]
EDITION = sys.argv[3] if len(sys.argv) > 3 else "standard"
if EDITION not in ("standard", "enterprise"):
    raise SystemExit(f"the edition is standard or enterprise, not {EDITION}")
PROTOS = "crates/hidane-proto/proto"
LIMIT = 1_048_487

i = lambda n: {"integerValue": str(n)}
arr = lambda *v: {"arrayValue": {"values": list(v)}}
mp = lambda fields: {"mapValue": {"fields": fields}}
string = lambda n: {"stringValue": "x" * n}


def nest(levels, bottom=None):
    v = bottom or i(1)
    for _ in range(levels):
        v = mp({"m": v})
    return v


def nest_arrays(levels):
    v = i(1)
    for _ in range(levels):
        v = arr(v)
    return v


def vector(*values):
    return mp({"__type__": {"stringValue": "__vector__"}, "value": arr(*({"doubleValue": v} for v in values))})


# Parts of one update write to document `c/<d>`: its document name, fields, mask paths,
# transforms or precondition. Field names and paths carry `d`, so a message tells which write
# it is about.
PARTS = {
    "a document name that does not parse": lambda d: ("name", d),
    "a reserved document ID": lambda d: ("name", f"c/__{d}__"),
    "a document ID over 1500 bytes": lambda d: ("name", "c/" + d * 1501),
    "a reserved field name": lambda d: ("fields", {f"__{d}__": i(1)}),
    "a reserved nested field name": lambda d: ("fields", {f"{d}_map": mp({f"__{d}__": i(1)})}),
    "an empty field name": lambda d: ("fields", {"": i(1)}),
    "a field name over 1500 bytes": lambda d: ("fields", {d * 1501: i(1)}),
    "a nested array": lambda d: ("fields", {f"{d}_array": arr(arr(i(1)))}),
    "a string over the limit": lambda d: ("fields", {f"{d}_string": string(LIMIT + 1)}),
    "21 levels of maps": lambda d: ("fields", {f"{d}_deep": nest(21)}),
    "a vector with NaN": lambda d: ("fields", {f"{d}_vector": vector("NaN")}),
    "a __type__ that is not a string": lambda d: ("fields", {f"{d}_type": mp({"__type__": i(1)})}),
    "a document over 1 MiB": lambda d: ("fields", {f"{d}_big1": string(600_000), f"{d}_big2": string(600_000)}),
    "a mask path that does not parse": lambda d: ("mask", [f"{d}..b"]),
    "a reserved mask path": lambda d: ("mask", [f"__{d}__"]),
    "a transform path that does not parse": lambda d: ("transforms", [{"fieldPath": f"{d}..t", "increment": i(1)}]),
    "a reserved transform path": lambda d: ("transforms", [{"fieldPath": f"__{d}__", "increment": i(1)}]),
    "an increment by a string": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "increment": {"stringValue": "x"}}]),
    "a reserved name in an array union": lambda d: ("transforms", [{"fieldPath": f"{d}_u", "appendMissingElements": {"values": [mp({f"__{d}__": i(1)})]}}]),
    "an update of a missing document": lambda d: ("precondition", {"exists": True}),
}

# More parts, each run against one kind of each stage (PROBES), and parts that put two
# problems in one field or one transform, each run alone.
MORE_PARTS = {
    "a __type__ that names no type": lambda d: ("fields", {f"{d}_type2": mp({"__type__": {"stringValue": "__foo__"}})}),
    "a vector without values": lambda d: ("fields", {f"{d}_vector2": mp({"__type__": {"stringValue": "__vector__"}})}),
    "an empty vector": lambda d: ("fields", {f"{d}_vector3": vector()}),
    "an empty nested field name": lambda d: ("fields", {f"{d}_map2": mp({"": i(1)})}),
    "a nested field name over 1500 bytes": lambda d: ("fields", {f"{d}_map3": mp({d * 1501: i(1)})}),
    "a maximum of an array": lambda d: ("transforms", [{"fieldPath": f"{d}_x", "maximum": arr(i(1))}]),
}
ALONE = {
    "a string over the limit, then a nested array, in one array": lambda d: ("fields", {f"{d}_mixed": arr(string(LIMIT + 1), arr(i(1)))}),
    "a nested array, then a string over the limit, in one array": lambda d: ("fields", {f"{d}_mixed": arr(arr(i(1)), string(LIMIT + 1))}),
    "a nested array 21 levels down": lambda d: ("fields", {f"{d}_deep": nest(21, arr(arr(i(1))))}),
    "a field name over 1500 bytes holding a nested array": lambda d: ("fields", {d * 1501: arr(arr(i(1)))}),
    "an empty field name holding a string over the limit": lambda d: ("fields", {"": string(LIMIT + 1)}),
    "an empty field name holding a nested array": lambda d: ("fields", {"": arr(arr(i(1)))}),
    "a reserved name in a map with a non-string __type__": lambda d: ("fields", {f"{d}_type": mp({"__type__": i(1), f"__{d}__": i(1)})}),
    "a vector with NaN and another key": lambda d: ("fields", {f"{d}_vector": mp({"__type__": {"stringValue": "__vector__"}, "value": arr({"doubleValue": "NaN"}), "other": i(1)})}),
    "a string operand at a transform path that does not parse": lambda d: ("transforms", [{"fieldPath": f"{d}..t", "increment": {"stringValue": "x"}}]),
    "a reserved name in an array union at a reserved path": lambda d: ("transforms", [{"fieldPath": f"__{d}__", "appendMissingElements": {"values": [mp({f"__{d}__": i(1)})]}}]),
    "a map holding a nested array, then a string over the limit": lambda d: ("fields", {f"{d}_map": mp({"a": arr(arr(i(1))), "b": string(LIMIT + 1)})}),
    "a map holding an empty field name": lambda d: ("fields", {f"{d}_map": mp({"": i(1)})}),
    "a map holding a vector with NaN": lambda d: ("fields", {f"{d}_map": mp({"v": vector("NaN")})}),
    "a map holding a __type__ that names no type": lambda d: ("fields", {f"{d}_map": mp({"t": mp({"__type__": {"stringValue": "__foo__"}})})}),
    "a map holding a vector without values": lambda d: ("fields", {f"{d}_map": mp({"v": mp({"__type__": {"stringValue": "__vector__"}})})}),
    "an array holding a map with an empty field name": lambda d: ("fields", {f"{d}_array": arr(mp({"": i(1)}))}),
    "an array holding a map with a string over the limit, then a nested array": lambda d: ("fields", {f"{d}_array": arr(mp({"s": string(LIMIT + 1)}), arr(i(1)))}),
    "an array holding a vector with NaN, then a string over the limit": lambda d: ("fields", {f"{d}_array": arr(vector("NaN"), string(LIMIT + 1))}),
    "an array holding a string over the limit, then a vector with NaN": lambda d: ("fields", {f"{d}_array": arr(string(LIMIT + 1), vector("NaN"))}),
    "a nested array 20 levels down": lambda d: ("fields", {f"{d}_deep": nest(20, arr(arr(i(1))))}),
    # The bounds of depth and of a nested name's length, which an enterprise database counts
    # otherwise.
    "20 levels of maps": lambda d: ("fields", {f"{d}_deep": nest(20)}),
    "19 levels of maps holding an array": lambda d: ("fields", {f"{d}_deep": nest(19, arr(i(1)))}),
    "20 levels of maps holding an array": lambda d: ("fields", {f"{d}_deep": nest(20, arr(i(1)))}),
    "18 levels of maps holding a nested array": lambda d: ("fields", {f"{d}_deep": nest(18, arr(arr(i(1))))}),
    "19 levels of maps holding a nested array": lambda d: ("fields", {f"{d}_deep": nest(19, arr(arr(i(1))))}),
    "20 levels of arrays": lambda d: ("fields", {f"{d}_deep": nest_arrays(20)}),
    "21 levels of arrays": lambda d: ("fields", {f"{d}_deep": nest_arrays(21)}),
    "a nested name of 1500 bytes in all": lambda d: ("fields", {"n": mp({"x" * 1498: i(1)})}),
    "a nested name of 1501 bytes in all": lambda d: ("fields", {"n": mp({"x" * 1499: i(1)})}),
    "a name in a map in an array, 1500 bytes in all": lambda d: ("fields", {"n": arr(mp({"x" * 1498: i(1)}))}),
    "a name in a map in an array, 1501 bytes in all": lambda d: ("fields", {"n": arr(mp({"x" * 1499: i(1)}))}),
    "a name in a map in an array named longer": lambda d: ("fields", {"nnnnnn": arr(mp({"x" * 1494: i(1)}))}),
    "a vector with NaN 21 levels down": lambda d: ("fields", {f"{d}_deep": nest(21, vector("NaN"))}),
    "a __type__ that is a string in a map with other keys": lambda d: ("fields", {f"{d}_type": mp({"__type__": {"stringValue": "x"}, "a": i(1)})}),
    "a __type__ that is null": lambda d: ("fields", {f"{d}_type": mp({"__type__": {"nullValue": "NULL_VALUE"}})}),
    "a __type__ that is a map": lambda d: ("fields", {f"{d}_type": mp({"__type__": mp({})})}),
    "a vector with a reserved key": lambda d: ("fields", {f"{d}_vector": mp({"__type__": {"stringValue": "__vector__"}, "value": arr({"doubleValue": 1.0}), f"__{d}__": i(1)})}),
    "a vector of integers": lambda d: ("fields", {f"{d}_vector": mp({"__type__": {"stringValue": "__vector__"}, "value": arr(i(1))})}),
    "a vector whose value is a string": lambda d: ("fields", {f"{d}_vector": mp({"__type__": {"stringValue": "__vector__"}, "value": {"stringValue": "x"}})}),
    "a field named ____": lambda d: ("fields", {"____": i(1)}),
    "a nested field named ____": lambda d: ("fields", {f"{d}_map": mp({"____": i(1)})}),
    "a field named __name__": lambda d: ("fields", {"__name__": i(1)}),
    "a reserved name in a map in an array": lambda d: ("fields", {f"{d}_array": arr(mp({f"__{d}__": i(1)}))}),
    "a bytes value over the limit": lambda d: ("fields", {f"{d}_bytes": {"bytesValue": "AAAA" * ((LIMIT + 3) // 3)}}),
    "a mask path __name__": lambda d: ("mask", ["__name__"]),
    "a mask path __name__.a": lambda d: ("mask", ["__name__.a"]),
    "a mask path a.__name__": lambda d: ("mask", ["a.__name__"]),
    "a mask path ____": lambda d: ("mask", ["____"]),
    "an empty mask path": lambda d: ("mask", [""]),
    "a mask path with a quoted reserved name in the middle": lambda d: ("mask", [f"{d}.`__{d}__`.`b.c`"]),
    "a mask path with a reserved name in the middle": lambda d: ("mask", [f"{d}.b.__{d}__.c.d"]),
    "an empty transform path": lambda d: ("transforms", [{"fieldPath": "", "increment": i(1)}]),
    "a transform path __name__": lambda d: ("transforms", [{"fieldPath": "__name__", "increment": i(1)}]),
    "a transform path with a reserved name in the middle": lambda d: ("transforms", [{"fieldPath": f"{d}.__{d}__.b", "increment": i(1)}]),
    "a transform without a type": lambda d: ("transforms", [{"fieldPath": f"{d}_t"}]),
    "a server value that is not the request time": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "setToServerValue": "SERVER_VALUE_UNSPECIFIED"}]),
    "a server value that is not the request time at a path that does not parse": lambda d: ("transforms", [{"fieldPath": f"{d}..t", "setToServerValue": "SERVER_VALUE_UNSPECIFIED"}]),
    "an increment by a map with a reserved name": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "increment": mp({f"__{d}__": i(1)})}]),
    "an increment by a map with a __type__ that is not a string": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "increment": mp({"__type__": i(1)})}]),
    "a minimum of null": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "minimum": {"nullValue": "NULL_VALUE"}}]),
    "a maximum of a nested array": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "maximum": arr(arr(i(1)))}]),
    "an array removal of a map with a reserved name": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "removeAllFromArray": {"values": [mp({f"__{d}__": i(1)})]}}]),
    "an array union of a string over the limit": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "appendMissingElements": {"values": [string(LIMIT + 1)]}}]),
    "an array union of an array": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "appendMissingElements": {"values": [arr(i(1))]}}]),
    "an array union of a vector with NaN": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "appendMissingElements": {"values": [vector("NaN")]}}]),
    "two transforms of one field": lambda d: ("transforms", [{"fieldPath": f"{d}_t", "increment": i(1)}, {"fieldPath": f"{d}_t", "increment": i(1)}]),
}
# Writes of other shapes, each run alone.
SHAPES = {
    "a delete with a mask": lambda d: [{"delete": doc(f"c/{d}"), "updateMask": {"fieldPaths": ["a"]}}],
    "a delete with transforms": lambda d: [{"delete": doc(f"c/{d}"), "updateTransforms": [{"fieldPath": "a", "increment": i(1)}]}],
    "a delete with a mask and transforms": lambda d: [{"delete": doc(f"c/{d}"), "updateMask": {"fieldPaths": ["a"]}, "updateTransforms": [{"fieldPath": "a", "increment": i(1)}]}],
    "a delete of a document name that does not parse, with a mask": lambda d: [{"delete": doc(d), "updateMask": {"fieldPaths": ["a"]}}],
    "a delete of a document ID over 1500 bytes": lambda d: [{"delete": doc("c/" + d * 1501)}],
    "a verify with a mask": lambda d: [{"verify": doc(f"c/{d}"), "currentDocument": {"exists": False}, "updateMask": {"fieldPaths": ["a"]}}],
    "a verify with a mask path that does not parse": lambda d: [{"verify": doc(f"c/{d}"), "currentDocument": {"exists": False}, "updateMask": {"fieldPaths": ["a..b"]}}],
    "a verify with transforms": lambda d: [{"verify": doc(f"c/{d}"), "currentDocument": {"exists": False}, "updateTransforms": [{"fieldPath": "a", "increment": i(1)}]}],
    "a verify with a mask, without a precondition": lambda d: [{"verify": doc(f"c/{d}"), "updateMask": {"fieldPaths": ["a"]}}],
    "a verify with transforms, without a precondition": lambda d: [{"verify": doc(f"c/{d}"), "updateTransforms": [{"fieldPath": "a", "increment": i(1)}]}],
    "a verify of a document name that does not parse, without a precondition": lambda d: [{"verify": doc(d)}],
    "a verify of a document name that does not parse, with a mask": lambda d: [{"verify": doc(d), "updateMask": {"fieldPaths": ["a"]}}],
    "a verify of a document ID over 1500 bytes": lambda d: [{"verify": doc("c/" + d * 1501), "currentDocument": {"exists": False}}],
    "a standalone transform with a mask": lambda d: [{"transform": {"document": doc(f"c/{d}"), "fieldTransforms": [{"fieldPath": "a", "increment": i(1)}]}, "updateMask": {"fieldPaths": ["a"]}}],
    "a standalone transform with a mask, at a path that does not parse": lambda d: [{"transform": {"document": doc(f"c/{d}"), "fieldTransforms": [{"fieldPath": "a..b", "increment": i(1)}]}, "updateMask": {"fieldPaths": ["a"]}}],
    "a standalone transform with update transforms": lambda d: [{"transform": {"document": doc(f"c/{d}"), "fieldTransforms": [{"fieldPath": "a", "increment": i(1)}]}, "updateTransforms": [{"fieldPath": "a", "increment": i(1)}]}],
    "a standalone transform without field transforms": lambda d: [{"transform": {"document": doc(f"c/{d}")}}],
    "a standalone transform of a document name that does not parse, without field transforms": lambda d: [{"transform": {"document": doc(d)}}],
    "a standalone transform of a document ID over 1500 bytes": lambda d: [{"transform": {"document": doc("c/" + d * 1501), "fieldTransforms": [{"fieldPath": "a", "increment": i(1)}]}}],
    "a standalone transform with a reserved name in an array union": lambda d: [{"transform": {"document": doc(f"c/{d}"), "fieldTransforms": [{"fieldPath": "a", "appendMissingElements": {"values": [mp({f"__{d}__": i(1)})]}}]}}],
    "an update of a document name that does not parse, with a precondition": lambda d: [{"update": {"name": doc(d), "fields": {}}, "currentDocument": {"exists": True}}],
}


def doc(path):
    return "{base}/" + path


def update(parts, d):
    """One update write of `c/<d>` with `parts` (slot, value), merged in order."""
    name, fields, mask, transforms, precondition = f"c/{d}", {}, None, [], None
    for slot, value in parts:
        if slot == "name":
            name = value
        elif slot == "fields":
            fields.update(value)
        elif slot == "mask":
            mask = (mask or []) + value
        elif slot == "transforms":
            transforms += value
        else:
            precondition = value
    if not fields and not transforms:
        fields = {"v": i(1)}
    write = {"update": {"name": doc(name), "fields": fields}}
    if mask is not None:
        write["updateMask"] = {"fieldPaths": mask}
    if transforms:
        write["updateTransforms"] = transforms
    if precondition is not None:
        write["currentDocument"] = precondition
    return write


def one_write(parts):
    return {name: (lambda part: lambda d: [update([part(d)], d)])(part) for name, part in parts.items()}


# Kinds of problem that take writes of their own: one or more writes to `c/<d>`.
KINDS = one_write(PARTS)
KINDS.update({
    "an empty write": lambda d: [{}],
    "a verify without a precondition": lambda d: [{"verify": doc(f"c/{d}")}],
    "a standalone transform path that does not parse": lambda d: [
        {"transform": {"document": doc(f"c/{d}"), "fieldTransforms": [{"fieldPath": f"{d}..t", "increment": i(1)}]}}],
    "a verify, then a write of the same document": lambda d: [
        {"verify": doc(f"c/{d}"), "currentDocument": {"exists": True}}, update([], d)],
    "a write, then a verify of the same document": lambda d: [
        update([], d), {"verify": doc(f"c/{d}"), "currentDocument": {"exists": True}}],
    "two verifies of the same document": lambda d: [
        {"verify": doc(f"c/{d}"), "currentDocument": {"exists": True}},
        {"verify": doc(f"c/{d}"), "currentDocument": {"exists": True}}],
    "a delete, then an update of the same document": lambda d: [
        {"delete": doc(f"c/{d}")}, update([("precondition", {"exists": True})], d)],
    "two update times for the same document": lambda d: [
        update([("precondition", {"updateTime": "2001-01-01T00:00:00Z"})], d),
        update([("precondition", {"updateTime": f"20{ord(d) - 87:02d}-01-01T00:00:00Z"})], d)],
})
# One kind of each stage, for MORE_PARTS.
PROBES = ["a verify, then a write of the same document", "a reserved field name", "an increment by a string",
          "a delete, then an update of the same document", "a nested array", "an update of a missing document",
          "a document over 1 MiB"]


def compress(text):
    """Runs of 200 or more of one character as `{repeat:<c>:<n>}`."""
    return re.sub(r"(.)\1{199,}", lambda m: f"{{repeat:{m.group(1)}:{len(m.group(0))}}}", text)


def fill(writes, database):
    return json.loads(json.dumps(writes).replace("{base}", f"{database}/documents"))


def rest(method, path, body=None):
    req = urllib.request.Request(
        f"http://{HOST}{path}", method=method, data=None if body is None else json.dumps(body).encode(),
        headers={"Content-Type": "application/json", "Authorization": "Bearer owner"})
    try:
        with urllib.request.urlopen(req, timeout=60) as res:
            raw = res.read()
            return res.status, json.loads(raw) if raw else None
    except urllib.error.HTTPError as err:
        raw = err.read()
        try:
            return err.code, json.loads(raw)
        except ValueError:
            return err.code, raw.decode(errors="replace")


CODES = ["OK", "CANCELLED", "UNKNOWN", "INVALID_ARGUMENT", "DEADLINE_EXCEEDED", "NOT_FOUND",
         "ALREADY_EXISTS", "PERMISSION_DENIED", "RESOURCE_EXHAUSTED", "FAILED_PRECONDITION", "ABORTED",
         "OUT_OF_RANGE", "UNIMPLEMENTED", "INTERNAL", "UNAVAILABLE", "DATA_LOSS", "UNAUTHENTICATED"]


def rest_outcome(status, body):
    if status != 200:
        error = body["error"]
        return {"status": status, "code": error["status"], "message": compress(error["message"])}
    if "status" in body:  # batchWrite: a status per write
        return {"status": status, "code": "OK", "statuses": [
            {"code": CODES[s.get("code", 0)], "message": compress(s.get("message", ""))} for s in body["status"]]}
    return {"status": status, "code": "OK", "results": len(body.get("writeResults", []))}


def grpcurl(rpc, messages, database=None):
    """Sends `messages` to `rpc`; returns the messages printed and the status (None for OK).
    grpcurl sends the last message when its input closes, which is at once here."""
    args = ["grpcurl", "-plaintext", "-max-time", "30", "-import-path", PROTOS, "-proto",
            "google/firestore/v1/firestore.proto", "-H", "authorization: Bearer owner"]
    if database:
        args += ["-H", f"google-cloud-resource-prefix: {database}"]
    args += ["-d", "@", HOST, f"google.firestore.v1.Firestore/{rpc}"]
    out = subprocess.run(args, input="\n".join(json.dumps(m) for m in messages), capture_output=True,
                         text=True, timeout=60)
    output = out.stdout + out.stderr
    decoder, received, at = json.JSONDecoder(), [], 0
    while True:
        while at < len(output) and output[at].isspace():
            at += 1
        if at == len(output) or output[at] != "{":
            break
        message, at = decoder.raw_decode(output, at)
        received.append(message)
    rest_ = output[at:]
    if not rest_.startswith("ERROR:"):
        assert not rest_.strip(), rest_
        return received, None
    code = rest_.split("Code: ", 1)[1].split("\n", 1)[0]
    message = rest_.split("Message: ", 1)[1].rstrip("\n") if "Message: " in rest_ else ""
    return received, {"code": re.sub(r"(?<!^)(?=[A-Z])", "_", code).upper(), "message": compress(message)}


def grpc_commit(database, writes):
    received, status = grpcurl("Commit", [{"database": database, "writes": writes}])
    if status:
        return status
    return {"code": "OK", "results": len(received[0].get("writeResults", []))}


def write_stream(database, writes):
    received, status = grpcurl("Write", [{"database": database}, {"streamToken": "MA==", "writes": writes}], database)
    if status:
        return status
    return {"code": "OK", "results": len(received[1].get("writeResults", []))}


TRANSPORTS = {
    "commit": lambda db, writes: rest_outcome(*rest("POST", f"/v1/{db}/documents:commit", {"writes": writes})),
    "grpc_commit": grpc_commit,
    "batch_write": lambda db, writes: rest_outcome(*rest("POST", f"/v1/{db}/documents:batchWrite", {"writes": writes})),
    "write_stream": write_stream,
}
REST_ONLY = ["commit", "batch_write"]

order = []


def record(name, writes, transports=TRANSPORTS):
    project = f"order-{len(order) + 1}"
    outcomes = {}
    for transport in transports:
        database = f"projects/{project}-{transport.replace('_', '-')}/databases/(default)"
        outcomes[transport] = TRANSPORTS[transport](database, fill(writes, database))
    order.append({"name": name, "project": project,
                  "writes": json.loads(compress(json.dumps(writes))), "outcomes": outcomes})


def two_writes(first, second, kinds=KINDS):
    record(f"{first} (write 1), then {second} (write 2)", kinds[first]("a") + kinds[second]("b"))


def in_one_write(first, second, parts):
    a, b = parts[first]("a"), parts[second]("a")
    record(f"{first} and {second} in one write", [update([a, b], "a")],
           REST_ONLY if a[0] == b[0] == "fields" else TRANSPORTS)


for first in KINDS:
    for second in KINDS:
        two_writes(first, second)

# Two parts of one write: parts in different places once, two sets of fields, masks or
# transforms in both orders. Two document names do not fit in one write.
names = list(PARTS)
for x, first in enumerate(names):
    for second in names[x + 1:]:
        a, b = PARTS[first]("a"), PARTS[second]("a")
        if a[0] == b[0] == "name":
            continue
        in_one_write(first, second, PARTS)
        if a[0] == b[0]:
            in_one_write(second, first, PARTS)

both = {**KINDS, **one_write(MORE_PARTS)}
every_part = {**PARTS, **MORE_PARTS}
for part in MORE_PARTS:
    for probe in PROBES:
        two_writes(part, probe, both)
        two_writes(probe, part, both)
    for other in ["a reserved field name", "a nested array", "a string over the limit", "a mask path that does not parse"]:
        in_one_write(part, other, every_part)
        if every_part[other]("a")[0] == MORE_PARTS[part]("a")[0]:
            in_one_write(other, part, every_part)
for name, part in ALONE.items():
    record(name, [update([part("a")], "a")])
for name, writes in SHAPES.items():
    record(name, writes("a"))


# A write to a document of another database (#203).
OWN = "projects/other-db/databases/(default)"
NAMED = "projects/other-db/databases/db1"
OTHER_PROJECT = "projects/other-project/databases/(default)"
IMPORTED = "projects/other-db-import/databases/db1"


def named(path, database=NAMED):
    return f"{database}/documents/{path}"


def set_(name, n):
    return {"update": {"name": name, "fields": {"v": i(n)}}}


steps = []


def step(name, op, request, outcome):
    steps.append({"name": name, "op": op, **request, "outcome": outcome})


written = []
for label, op, path in [("REST :commit", "commit", "c/rest"), ("gRPC Commit", "grpc_commit", "c/grpc"),
                        ("REST :batchWrite", "batch_write", "c/batch"), ("the Write stream", "write_stream", "c/stream")]:
    writes = [set_(named(path), 1)]
    step(f"{label} writes a document of another database", op, {"database": OWN, "writes": writes},
         TRANSPORTS[op](OWN, writes))
    written.append(named(path))

status, body = rest("POST", f"/v1/{OWN}/documents:beginTransaction", {})
assert status == 200, body
writes = [set_(named("c/transaction"), 1)]
step("a transaction's commit writes a document of another database", "transaction",
     {"database": OWN, "writes": writes},
     rest_outcome(*rest("POST", f"/v1/{OWN}/documents:commit", {"transaction": body["transaction"], "writes": writes})))
written.append(named("c/transaction"))

writes = [set_(named("c/own", OWN), 1), set_(named("c/both"), 2)]
step("a commit writes a document of its database and one of another", "commit", {"database": OWN, "writes": writes},
     TRANSPORTS["commit"](OWN, writes))
written.append(named("c/both"))

writes = [set_(named("c/project", OTHER_PROJECT), 1)]
step("a commit writes a document of another project", "commit", {"database": OWN, "writes": writes},
     TRANSPORTS["commit"](OWN, writes))


def query(database):
    status, rows = rest("POST", f"/v1/{database}/documents:runQuery", {"structuredQuery": {"from": [{"collectionId": "c"}]}})
    assert status == 200, rows
    return {"status": status, "documents": [r["document"]["name"] for r in rows if "document" in r]}


def batch_get(database, documents):
    status, rows = rest("POST", f"/v1/{database}/documents:batchGet", {"documents": documents})
    if status != 200:
        return rest_outcome(status, rows)
    return {"status": status, "found": [r["found"]["name"] for r in rows if "found" in r],
            "missing": [r["missing"] for r in rows if "missing" in r]}


for name in written + [named("c/project", OTHER_PROJECT), named("c/own", OWN)]:
    status, body = rest("GET", f"/v1/{name}")
    step(f"GET {name}", "get", {"document": name}, {"status": status})
step("a query of the request's database", "query", {"database": OWN}, query(OWN))
step("a query of the other database", "query", {"database": NAMED}, query(NAMED))
step("a batchGet in the request's database of the documents it wrote", "batch_get",
     {"database": OWN, "documents": written}, batch_get(OWN, written))
step("a batchGet in the other database of the same documents", "batch_get",
     {"database": NAMED, "documents": written}, batch_get(NAMED, written))

status, body = rest("POST", "/emulator/v1/projects/other-db:export",
                    {"database": OWN, "export_directory": EXPORT_DIR, "export_name": "own"})
assert status == 200, body
status, body = rest("POST", "/emulator/v1/projects/other-db-import:import",
                    {"database": IMPORTED, "export_directory": f"{EXPORT_DIR}/own/own.overall_export_metadata"})
assert status == 200, body
step("an export of the request's database, imported into a database of the other name", "export_import",
     {"database": OWN, "import": IMPORTED}, query(IMPORTED))

# One case per line.
dump = lambda value: json.dumps(value, ensure_ascii=False)
print("{")
print(f' "oracle": {dump("cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)")},')
print(f' "generated_by": {dump("tools/oracle/commit_checks.py")},')
if EDITION != "standard":
    print(f' "edition": {dump(EDITION)},')
print(' "order": [\n' + ",\n".join("  " + dump(case) for case in order) + "\n ],")
print(' "other_database": [\n' + ",\n".join("  " + dump(s) for s in steps) + "\n ]")
print("}")
