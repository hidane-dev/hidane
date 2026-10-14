#!/usr/bin/env python3
"""How do Security Rules check a commit that writes one document more than once? (#262)

Usage (from the repository root), with the jar started under an English locale
(`java -Duser.language=en -Duser.country=US -jar …`):
    python3 -I tools/oracle/rules_same_document.py 127.0.0.1:8086 \\
        > crates/hidane/tests/fixtures/rules_same_document.json

The official emulator collapses a commit's writes to one document into one and checks that
once, where the first of them comes. Each case loads rules into a project of its own whose
condition holds only if the check sees what the case says, seeds the document as the owner,
and commits the writes without an `Authorization` header; the status and message are recorded.

- `resource` is the document before the commit, and `request.resource` what the last write
  leaves.
- The method: a delete last makes a `delete`; otherwise the first write decides, by its
  `exists` precondition or by whether the document exists (an upsert), and a first delete by
  its precondition or the same.
- `request.writeFields`: the paths of the transforms in order, then those of the masks joined
  as one mask (in the official emulator's order); a write without a mask sets the whole
  document, and a delete starts over. `request.transforms`: the transforms in order.
- Two documents written in turn: the check of the first one written comes first.

Stdlib only.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from steps import OWNER, call, case, dump, rules  # noqa: E402

I = lambda n: {"integerValue": str(n)}
count = [0]


def start(name, statements, seed=None):
    """A case in a project of its own under `statements` (lines of `match /r/{id}`, or a whole
    `match` block when they start with `match`), with `r/d` seeded with `seed` (a map of fields)."""
    count[0] += 1
    project = f"same-document-{count[0]}"
    case(name, project=project)
    body = "\n".join(statements) if statements and statements[0].startswith("match") else (
        "match /r/{id} {\n" + "\n".join("  " + s for s in statements) + "\n}")
    rules(project, "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
          + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")
    docs = f"projects/{project}/databases/(default)/documents"
    if seed is not None:
        call("PATCH", f"/v1/{docs}/r/d", {"fields": seed}, auth=OWNER)
    return docs


def commit(docs, *writes):
    call("POST", f"/v1/{docs}:commit", {"writes": [w(docs) for w in writes]})


def put(fields, mask=None, exists=None, transforms=None, doc="r/d"):
    def write(docs):
        w = {"update": {"name": f"{docs}/{doc}", "fields": fields}}
        if mask is not None:
            w["updateMask"] = {"fieldPaths": mask}
        if exists is not None:
            w["currentDocument"] = {"exists": exists}
        if transforms:
            w["updateTransforms"] = [{"fieldPath": f, "increment": I(1)} for f in transforms]
        return w
    return write


def transform(*paths, doc="r/d"):
    return lambda docs: {"transform": {"document": f"{docs}/{doc}",
                                       "fieldTransforms": [{"fieldPath": f, "increment": I(1)} for f in paths]}}


def delete(doc="r/d"):
    return lambda docs: {"delete": f"{docs}/{doc}"}


NONE = "allow create, update, delete: if false;"

# What the check sees.
docs = start("set, set: resource is the stored document",
             ["allow update: if resource.data == {'n': 3} && request.resource.data == {};"], {"n": I(3)})
commit(docs, put({"n": I(0)}), put({}))
docs = start("set, set: not the document after the first write",
             ["allow update: if resource.data == {'n': 0};"], {"n": I(3)})
commit(docs, put({"n": I(0)}), put({}))
docs = start("mask, mask: request.resource is what the last write leaves",
             ["allow update: if request.resource.data == {'n': 7, 'm': 8, 'k': 9};"], {"n": I(3), "k": I(9)})
commit(docs, put({"n": I(7)}, ["n"]), put({"m": I(8)}, ["m"]))

# The method.
docs = start("set, delete, of a stored document: a delete",
             ["allow delete: if resource.data == {'n': 3} && request.resource == null;",
              "allow create, update: if false;"], {"n": I(3)})
commit(docs, put({"n": I(5)}), delete())
docs = start("set, delete, of a missing document: a delete no statement matches",
             ["allow create, update: if true;"])
commit(docs, put({"n": I(5)}), delete())
docs = start("delete, set, of a stored document: an update",
             ["allow update: if resource.data == {'n': 3} && request.resource.data == {'m': 1};",
              "allow create, delete: if false;"], {"n": I(3)})
commit(docs, delete(), put({"m": I(1)}))
docs = start("delete, set, of a missing document: a create",
             ["allow create: if resource == null && request.resource.data == {'n': 1};",
              "allow update, delete: if false;"])
commit(docs, delete(), put({"n": I(1)}))
docs = start("delete with exists, set, of a missing document: an update",
             ["allow update: if request.resource.data == {'n': 1};", "allow create, delete: if false;"])
commit(docs, lambda d: {**delete()(d), "currentDocument": {"exists": True}}, put({"n": I(1)}))
docs = start("create, mask: a create",
             ["allow create: if request.resource.data == {'n': 1, 'm': 2};", "allow update, delete: if false;"])
commit(docs, put({"n": I(1)}, exists=False), put({"m": I(2)}, ["m"]))
docs = start("update by exists, delete, set: an update",
             ["allow update: if resource.data == {'n': 3} && request.resource.data == {'m': 2};",
              "allow create, delete: if false;"], {"n": I(3)})
commit(docs, put({"n": I(1)}, exists=True), delete(), put({"m": I(2)}))
docs = start("set, set, denied: one check",
             ["allow create: if false;", "allow update: if request.resource.data.n == 1;", "allow delete: if false;"],
             {"n": I(3)})
commit(docs, put({"n": I(1)}), put({"n": I(2)}))

# request.writeFields and request.transforms.
for name, writes, fields, transforms in [
    ("masks join", [put({"a": I(1), "z": I(1)}, ["z", "a"]), put({"c": I(1)}, ["n", "c"])], "['a', 'c', 'z', 'n']", "null"),
    ("a mask under another", [put({"a": {"mapValue": {"fields": {"b": I(1)}}}}, ["a.b"]), put({"a": I(8)}, ["a"])], "['a']", "null"),
    ("a set, then a mask", [put({}), put({"m": I(8)}, ["m"])], "null", "null"),
    ("a mask, a delete, a mask", [put({"n": I(7)}, ["n"]), delete(), put({"a": I(8)}, ["a"])], "null", "null"),
    ("transforms, then a mask", [transform("k"), put({"n": I(7)}, ["n"])], "['k', 'n']", "['k']"),
    ("a mask with a transform, then a set", [put({"n": I(7)}, ["n"], transforms=["k"]), put({})], "['k']", "['k']"),
    ("a set, then a transform", [put({}), transform("z")], "['z']", "['z']"),
    ("transforms in turn", [transform("z"), transform("a"), transform("z")], "['z', 'a', 'z']", "['z', 'a', 'z']"),
    ("a transform, a delete, a set", [transform("k"), delete(), put({})], "null", "null"),
    ("a transform, a delete, a transform", [transform("k"), delete(), transform("z")], "['z']", "['z']"),
]:
    docs = start(name, [f"allow create, update: if request.writeFields == {fields} && request.transforms == {transforms};",
                        "allow delete: if false;"], {"n": I(3), "k": I(9)})
    commit(docs, *writes)

# Two documents written in turn: q/a first, then r/d, then q/a again; both denied.
docs = start("two documents in turn: the first one's check",
             ["match /q/{id} {\n  allow create, update: if false;\n}",
              "match /r/{id} {\n  allow create: if false;\n  allow update: if false;\n}"])
commit(docs, put({"n": I(1)}, doc="q/a"), put({"n": I(1)}), put({"n": I(2)}, doc="q/a"))
dump()
