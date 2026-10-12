#!/usr/bin/env python3
"""Record how the official emulator answers a commit that writes one document more than once
(#74).

Usage: python3 -I tools/oracle/same_document_writes.py HOST:PORT > fixture.json

The official emulator stores a document as a Datastore entity and collapses a commit's writes to
one document into one mutation, which refuses some sequences with INVALID_ARGUMENT before any
write is applied: "Cannot delete then update an entity in the same request." and the like. Each
write is an insert (an `exists: false` precondition), an update (`exists: true` or an
`updateTime`), an upsert (no precondition, with or without a mask, or a standalone transform) or
a delete, and the collapsed mutation so far decides what may follow. Every pair of kinds runs on
a document that exists and on one that does not, then longer sequences that show what the
collapsed mutation is, preconditions on later writes, and which error comes first when the
commit has another problem. Each case reads the document back.

firebase-js-sdk's transaction tests send such commits (a delete then an update of one document
must fail with `invalid-argument`). The Write stream answers the same (checked with a web SDK
batch in Node). Of two problems in one commit, the official emulator reports a nested array or a
transform path that does not parse in another order than hidane (#202), so no case here has
one. Stdlib only.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from harness import case, cases, commit, delete, update  # noqa: E402

i = lambda n: {"integerValue": str(n)}
T1 = "2001-01-01T00:00:00Z"
T2 = "2002-01-01T00:00:00.000001Z"


def write(kind, name, n):
    """One write of `kind` to `name`, setting `v` to `n` (a transform increments `t`)."""
    fields = {"v": i(n)}
    return {
        "set": lambda: update(name, fields),
        "merge": lambda: update(name, fields, mask=["v"]),
        "update": lambda: update(name, fields, mask=["v"], exists=True),
        "create": lambda: update(name, fields, exists=False),
        "delete": lambda: delete(name),
        "transform": lambda: {"transform": {"document": "{base}/" + name,
                                            "fieldTransforms": [{"fieldPath": "t", "increment": i(1)}]}},
        "set-exists": lambda: update(name, fields, exists=True),
        "update-time": lambda: update(name, fields, mask=["v"], update_time=T1),
        "update-time-2": lambda: update(name, fields, mask=["v"], update_time=T2),
        "delete-exists": lambda: delete(name, exists=True),
        "delete-time": lambda: {**delete(name), "currentDocument": {"updateTime": T1}},
    }[kind]()


def sequence(kinds, existing, project):
    steps = [commit(update("c/d", {"v": i(0)}))] if existing else []
    steps.append(commit(*(write(k, "c/d", n) for n, k in enumerate(kinds, 1))))
    steps.append(("GET", "{base}/c/d", None))
    state = "an existing" if existing else "a missing"
    case(f"{' then '.join(kinds)} on {state} document", project, steps)


KINDS = ["set", "merge", "update", "create", "delete", "transform"]
n = 0
for existing in (True, False):
    for first in KINDS:
        for second in KINDS:
            n += 1
            sequence([first, second], existing, f"s-pair-{n}")

# What the collapsed mutation is: an insert or an update stays one under upserts and updates, a
# delete then an insert or an upsert is an upsert, and anything then a delete is a delete.
for kinds in [
    ["set", "update", "create"], ["update", "set", "create"], ["update", "update", "create"],
    ["create", "set", "create"], ["create", "update", "create"], ["delete", "set", "create"],
    ["delete", "create", "create"], ["delete", "create", "update"], ["delete", "delete", "update"],
    ["set", "delete", "update"], ["create", "delete", "update"], ["set", "delete", "create"],
    ["update", "delete", "create"], ["create", "delete", "create"], ["update", "delete", "update"],
    ["delete", "transform", "update"],
]:
    for existing in (True, False):
        n += 1
        sequence(kinds, existing, f"s-seq-{n}")

# Preconditions on later writes: a delete that needs the document cannot follow a delete; an
# update time can only repeat the first write's (or the first write's alone carries one). The
# document is missing, so a precondition that is checked fails with stored version 0.
for kinds in [
    ["set-exists", "create"], ["delete", "set-exists"], ["update-time", "create"],
    ["delete", "update-time"], ["delete-time", "update"], ["delete-exists", "update"],
    ["delete", "delete-exists"], ["delete-exists", "delete-exists"], ["set", "delete-exists"],
    ["delete-exists", "set"], ["set", "update-time"], ["update", "update-time"],
    ["create", "update-time"], ["delete", "delete-time"], ["update-time", "update-time-2"],
    ["update-time", "set", "update-time-2"], ["update-time", "set"], ["update-time", "delete-time"],
    ["update-time", "update-time"],
]:
    n += 1
    sequence(kinds, False, f"s-pre-{n}")

# Which error comes first: the sequence is checked over the whole commit before any
# precondition, after the names, masks and field paths of every write.
case("an insert that fails, then a refused sequence on another document", "s-order-1", [
    commit(update("c/a", {"v": i(0)})),
    commit(update("c/a", {"v": i(1)}, exists=False), delete("c/b"), update("c/b", {"v": i(2)}, mask=["v"], exists=True)),
])
case("a refused sequence, then an insert that fails on another document", "s-order-2", [
    commit(update("c/a", {"v": i(0)})),
    commit(delete("c/b"), update("c/b", {"v": i(2)}, mask=["v"], exists=True), update("c/a", {"v": i(1)}, exists=False)),
])
case("a mask that does not parse, then a refused sequence", "s-order-3", [
    commit(update("c/a", {"v": i(1)}, mask=["a..b"]), delete("c/b"), update("c/b", {"v": i(2)}, mask=["v"], exists=True)),
])
case("a refused sequence, then a mask that does not parse", "s-order-4", [
    commit(delete("c/b"), update("c/b", {"v": i(2)}, mask=["v"], exists=True), update("c/a", {"v": i(1)}, mask=["a..b"])),
])
case("a refused sequence whose update has a reserved field name", "s-order-5", [
    commit(delete("c/b"), update("c/b", {"__x__": i(2)}, mask=["__x__"], exists=True)),
])
case("a refused sequence, then a document name that does not parse", "s-order-6", [
    commit(delete("c/b"), update("c/b", {"v": i(2)}, mask=["v"], exists=True), update("c", {})),
])
case("a verify, then a refused sequence of the same document", "s-order-7", [
    commit(update("c/b", {"v": i(0)})),
    commit({"verify": "{base}/c/b", "currentDocument": {"exists": True}}, delete("c/b"),
           update("c/b", {"v": i(2)}, mask=["v"], exists=True)),
])
case("the same documents in two commits", "s-order-8", [
    commit(delete("c/b"), update("c/c", {"v": i(1)})),
    commit(update("c/b", {"v": i(2)}, mask=["v"], exists=True)),
    ("GET", "{base}/c/b", None),
])

json.dump({
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/same_document_writes.py",
    "cases": cases,
}, sys.stdout, ensure_ascii=False, indent=1)
print()
