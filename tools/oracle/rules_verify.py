#!/usr/bin/env python3
"""How are the `verify` writes of a commit checked against Security Rules? (#173)

Usage (from the repository root), with a freshly started jar under an English locale whose
standard output goes to a file, as `rules_coverage.py` takes it:
    (cd /tmp && java -Duser.language=en -Duser.country=US -jar …/cloud-firestore-emulator-v1.22.0.jar \\
        --host 127.0.0.1 --port 8086 > emulator.out) &
    python3 -I tools/oracle/rules_verify.py 127.0.0.1:8086 /tmp/emulator.out \\
        > crates/hidane/tests/fixtures/rules_verify.json

A verify (`Write.verify`, which the SDKs send for a document a transaction read without writing
it) is checked as a `get` of its document. Each case loads rules into a project of its own,
writes its documents as the owner and commits over REST without an `Authorization` header,
recording the status and the message (and what `debug()` printed, `log`). The cases:

- a verify alone, of a document the rules let be read and of one they do not, of a missing
  document with `exists: false` and with `exists: true` (the rules come before the
  precondition), with an `update_time` precondition that fails, under `allow read`, `allow list`
  and `allow write`; what `resource` and `request.resource` hold, and `debug()` of both (in the
  second pass of the official evaluation, `request` has the keys of a write);
- a verify next to other writes, allowed and denied, in either order: the first write denied in
  order decides, and a denial lists the statements of the writes checked before it too, as
  `false`; preconditions that fail next to writes the rules deny (the rules of the whole commit
  come first), and the checks that come before the rules (a document verified twice, verified
  and written in either order, a verify without a precondition, with a mask or a transform);
- verifies in a transaction: allowed, denied (which ends the transaction), and next to a write;
- lookups: the limits of 10 per write and 20 per commit, which verifies share with the other
  writes; the verified document costs a write's `exists()` a lookup, and a document the commit
  writes costs a verify's, while `existsAfter()` of either is free; `get()` and `getAfter()` in
  a verify's condition;
- a BatchWrite with a verify, which is refused to anyone but the owner.

A transaction's ID is the one `beginTransaction` answered before. Stdlib only.
"""

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import steps  # noqa: E402
from steps import OWNER, call, case, dump, rules  # noqa: E402

steps.LOG = sys.argv[2]
I = lambda n: {"integerValue": str(n)}
P = "/databases/$(database)/documents"
# A time no document was written at.
LONG_AGO = "2000-01-01T00:00:00Z"
count = [0]


def start(name, body, docs=(("c/a", 1), ("c/b", 2)), **extra):
    """A case of its own project: its rules (`match` statements below the documents), and
    `docs` written as the owner; returns the documents' path."""
    count[0] += 1
    project = f"verify-{count[0]}"
    case(name, project=project, **extra)
    rules(project, "rules_version = '2';\nservice cloud.firestore {\n  match /databases/{database}/documents {\n"
          + "\n".join("    " + line for line in body.split("\n")) + "\n  }\n}\n")
    base = f"projects/{project}/databases/(default)/documents"
    for path, n in docs:
        parent, id_ = path.rsplit("/", 1)
        status, _ = call("POST", f"/v1/{base}/{parent}?documentId={id_}", {"fields": {"n": I(n)}}, auth=OWNER)
        assert status == 200, (path, status)
    return base


def database(base):
    return base.rsplit("/documents", 1)[0]


def commit(base, *writes, **extra):
    return call("POST", f"/v1/{database(base)}/documents:commit", {"writes": list(writes), **extra})


def verify(base, doc, **precondition):
    return {"verify": f"{base}/{doc}", "currentDocument": precondition or {"exists": True}}


def update(base, doc, n=1, **precondition):
    write = {"update": {"name": f"{base}/{doc}", "fields": {"n": I(n)}}}
    if precondition:
        write["currentDocument"] = precondition
    return write


def delete(base, doc):
    return {"delete": f"{base}/{doc}"}


def begin(base):
    status, text = call("POST", f"/v1/{database(base)}/documents:beginTransaction", {})
    assert status == 200, text
    return json.loads(text)["transaction"]


# A verify alone. `a` may be read, `b` may not; the missing `m` may be read, `z` may not.
READ = "match /c/{d} {\n  allow get: if d == 'a' || d == 'm';\n  allow write: if true;\n}"
for name, doc, precondition in [
    ("a readable document", "c/a", {"exists": True}),
    ("an unreadable document", "c/b", {"exists": True}),
    ("a readable missing document, exists: false", "c/m", {"exists": False}),
    ("an unreadable missing document, exists: false", "c/z", {"exists": False}),
    ("a readable missing document, exists: true", "c/m", {"exists": True}),
    ("an unreadable missing document, exists: true", "c/z", {"exists": True}),
    ("a readable document, update_time that does not match", "c/a", {"updateTime": LONG_AGO}),
    ("an unreadable document, update_time that does not match", "c/b", {"updateTime": LONG_AGO}),
    ("a readable missing document, update_time", "c/m", {"updateTime": LONG_AGO}),
    ("an unreadable missing document, update_time", "c/z", {"updateTime": LONG_AGO}),
]:
    base = start(f"verify alone: {name}", READ)
    commit(base, verify(base, doc, **precondition))

for name, body in [
    ("allow read", "match /c/{d} {\n  allow read: if true;\n}"),
    ("allow list", "match /c/{d} {\n  allow list: if true;\n  allow write: if true;\n}"),
    ("allow write", "match /c/{d} {\n  allow write: if true;\n}"),
    ("resource", "match /c/{d} {\n  allow get: if resource.data.n == 1;\n}"),
    ("request.resource is the stored document", "match /c/{d} {\n  allow get: if request.resource.data.n == 1;\n}"),
    ("request.resource and resource", "match /c/{d} {\n  allow get: if request.resource == resource;\n}"),
    ("request.method and request.inTransaction",
     "match /c/{d} {\n  allow get: if request.method == 'get' && request.inTransaction == true;\n}"),
    ("request.writeFields, request.transforms and request.readFields",
     "match /c/{d} {\n  allow get: if request.writeFields == null && request.transforms == null && request.readFields == null\n"
     "    && resource.data.n == 1;\n}"),
]:
    for doc in ("c/a", "c/b"):
        base = start(f"verify alone under {name}: {doc}", body)
        commit(base, verify(base, doc))

base = start("verify alone: request.resource of a missing document",
             "match /c/{d} {\n  allow get: if request.resource == null && resource == null;\n}")
commit(base, verify(base, "c/m", exists=False))

# What `debug()` prints. `resource` fails in the official first pass, before anything prints.
DEBUG = "match /c/{d} {\n  allow get: if debug([resource, request]) != null;\n  allow write: if true;\n}"
base = start("debug: a document", DEBUG)
commit(base, verify(base, "c/a"))
base = start("debug: a missing document", DEBUG)
commit(base, verify(base, "c/m", exists=False))
base = start("debug: next to a write", DEBUG)
commit(base, update(base, "c/x", 5), verify(base, "c/a"))
base = start("debug: in a transaction", DEBUG)
commit(base, verify(base, "c/a"), transaction=begin(base))
# Where the official first pass decides, `request` has the keys of a read.
base = start("debug: decided in the official first pass",
             "match /c/{d} {\n  allow get: if debug(request) != null;\n}", differs=True)
commit(base, verify(base, "c/a"))

# Next to other writes: `a` and the missing `m` may be read, `b` may not; `y` may be written,
# `x` may not, and nothing may be deleted.
BATCH = ("match /c/{d} {\n  allow get: if d == 'a' || d == 'm';\n  allow create, update: if d == 'y';\n"
         "  allow delete: if false;\n}")
for name, writes in [
    ("allowed verify, allowed write", lambda b: [verify(b, "c/a"), update(b, "c/y")]),
    ("allowed write, allowed verify", lambda b: [update(b, "c/y"), verify(b, "c/a")]),
    ("denied verify, allowed write", lambda b: [verify(b, "c/b"), update(b, "c/y")]),
    ("allowed write, denied verify", lambda b: [update(b, "c/y"), verify(b, "c/b")]),
    ("allowed verify, denied write", lambda b: [verify(b, "c/a"), update(b, "c/x")]),
    ("denied write, allowed verify", lambda b: [update(b, "c/x"), verify(b, "c/a")]),
    ("denied verify, denied write", lambda b: [verify(b, "c/b"), update(b, "c/x")]),
    ("denied write, denied verify", lambda b: [update(b, "c/x"), verify(b, "c/b")]),
    ("denied delete, denied verify, denied write", lambda b: [delete(b, "c/a"), verify(b, "c/b"), update(b, "c/x")]),
    ("two allowed verifies", lambda b: [verify(b, "c/a"), verify(b, "c/m", exists=False)]),
    ("allowed verify, denied verify", lambda b: [verify(b, "c/a"), verify(b, "c/b")]),
    ("denied verify, allowed verify", lambda b: [verify(b, "c/b"), verify(b, "c/a")]),
    # A precondition that fails comes after the rules of every write of the commit.
    ("failing verify, denied write", lambda b: [verify(b, "c/m"), update(b, "c/x")]),
    ("failing verify, allowed write", lambda b: [verify(b, "c/m"), update(b, "c/y")]),
    ("denied write, failing verify", lambda b: [update(b, "c/x"), verify(b, "c/m")]),
    ("failing denied verify, allowed write", lambda b: [verify(b, "c/z"), update(b, "c/y")]),
    ("allowed write, failing denied verify", lambda b: [update(b, "c/y"), verify(b, "c/z")]),
    ("failing write, denied verify", lambda b: [update(b, "c/y", exists=True), verify(b, "c/b")]),
    ("failing write, allowed verify", lambda b: [update(b, "c/y", exists=True), verify(b, "c/a")]),
    ("failing write, denied write", lambda b: [update(b, "c/y", exists=True), update(b, "c/x")]),
    ("two failing verifies", lambda b: [verify(b, "c/m"), verify(b, "c/a", exists=False)]),
]:
    base = start(f"batch: {name}", BATCH)
    commit(base, *writes(base))

# Checks that come before the rules, which deny everything here.
NOTHING = "match /c/{d} {\n  allow read, write: if false;\n}"
for name, writes in [
    ("a document verified twice", lambda b: [verify(b, "c/a"), verify(b, "c/a")]),
    ("a document verified and written", lambda b: [verify(b, "c/a"), update(b, "c/a")]),
    ("a document written and verified", lambda b: [update(b, "c/a"), verify(b, "c/a")]),
    ("a verify without a precondition", lambda b: [{"verify": f"{b}/c/a"}]),
    ("a verify with a mask", lambda b: [{**verify(b, "c/a"), "updateMask": {"fieldPaths": ["n"]}}]),
    ("a verify with a transform", lambda b: [{**verify(b, "c/a"), "updateTransforms": [
        {"fieldPath": "n", "increment": I(1)}]}]),
]:
    base = start(f"invalid: {name}", NOTHING)
    commit(base, *writes(base))

# Transactions, in one project: its transaction IDs do not repeat.
base = start("transactions", READ)
commit(base, verify(base, "c/a"), transaction=begin(base))
transaction = begin(base)
commit(base, verify(base, "c/b"), transaction=transaction)
commit(base, verify(base, "c/a"), transaction=transaction)
commit(base, update(base, "c/x", 3), verify(base, "c/b"), transaction=begin(base))
commit(base, update(base, "c/x", 3), verify(base, "c/a"), transaction=begin(base))

# Lookups: `exists()` of distinct missing documents, or of the write's own (`$(d)` in the path).
distinct = lambda n: " && ".join(f"!exists({P}/missing/m{i})" for i in range(n))
own = lambda n: " && ".join(f"!exists({P}/missing/$(d + '_{i}'))" for i in range(n))
for n in (10, 11):
    base = start(f"lookups: a verify, {n} distinct exists", f"match /c/{{d}} {{\n  allow get: if {distinct(n)};\n}}")
    commit(base, verify(base, "c/a"))
for n in (10, 11):
    base = start(f"lookups: two verifies, {n} exists of their own documents in each",
                 f"match /c/{{d}} {{\n  allow get: if {own(n)};\n}}")
    commit(base, verify(base, "c/a"), verify(base, "c/b"))
for n in (10, 11):
    base = start(f"lookups: a verify and a write, {n} exists of their own documents in each",
                 f"match /c/{{d}} {{\n  allow get, write: if {own(n)};\n}}")
    commit(base, verify(base, "c/a"), update(base, "c/w1"))
for n in (6, 7):
    for order, writes in [
        ("a verify and two writes", lambda b: [verify(b, "c/a"), update(b, "c/w1"), update(b, "c/w2")]),
        ("two writes and a verify", lambda b: [update(b, "c/w1"), update(b, "c/w2"), verify(b, "c/a")]),
        ("two writes", lambda b: [update(b, "c/w1"), update(b, "c/w2")]),
    ]:
        base = start(f"lookups: {order}, {n} exists of their own documents in each",
                     f"match /c/{{d}} {{\n  allow get, write: if {own(n)};\n}}")
        commit(base, *writes(base))
# Which documents of the commit cost a lookup. A verify's own document is free. The verified
# document costs a write's `exists()` a lookup (whether the write comes first or not), and the
# documents the commit writes cost a verify's `exists()` one; `existsAfter()` of either is free.
# (Whether a document the commit writes costs another write's `exists()` a lookup varies with the
# project's name on the official emulator, so no case here depends on it.)
for name, writes, condition in [
    ("a verify's exists() of its own document", lambda b: [verify(b, "c/a")], f"exists({P}/c/$(d))"),
    ("a write's exists() of the verified document", lambda b: [update(b, "c/w1"), verify(b, "c/a")], None),
    ("a write's exists() of the verified document, verified first", lambda b: [verify(b, "c/a"), update(b, "c/w1")],
     None),
    ("a write's existsAfter() of the verified document", lambda b: [update(b, "c/w1"), verify(b, "c/a")], None),
    ("a verify's exists() of a written document", lambda b: [update(b, "c/b", 9), verify(b, "c/a")],
     f"exists({P}/c/b)"),
    ("a verify's exists() of a written document, verified first", lambda b: [verify(b, "c/a"), update(b, "c/b", 9)],
     f"exists({P}/c/b)"),
    ("a verify's existsAfter() of a written document", lambda b: [update(b, "c/b", 9), verify(b, "c/a")],
     f"existsAfter({P}/c/b)"),
]:
    if condition is None:
        lookup = "existsAfter" if "existsAfter" in name else "exists"
        body = (f"match /c/{{d}} {{\n  allow get: if true;\n"
                f"  allow write: if {lookup}({P}/c/a) && {distinct(10)};\n}}")
    else:
        body = f"match /c/{{d}} {{\n  allow get: if {condition} && {distinct(10)};\n  allow write: if true;\n}}"
    base = start(f"lookups: {name}", body)
    commit(base, *writes(base))
base = start("lookups: a verify's get() sees the documents before the commit",
             f"match /c/{{d}} {{\n  allow get: if get({P}/c/b).data.n == 2;\n  allow write: if true;\n}}")
commit(base, update(base, "c/b", 9), verify(base, "c/a"))
base = start("lookups: a verify's getAfter() sees the commit",
             f"match /c/{{d}} {{\n  allow get: if getAfter({P}/c/b).data.n == 9 && getAfter({P}/c/$(d)).data.n == 1;\n"
             "  allow write: if true;\n}")
commit(base, update(base, "c/b", 9), verify(base, "c/a"))

# BatchWrite needs the owner whatever the rules say.
base = start("batchWrite: a verify", "match /c/{d} {\n  allow read, write: if true;\n}")
call("POST", f"/v1/{database(base)}/documents:batchWrite", {"writes": [verify(base, "c/a")]})

dump()
