#!/usr/bin/env python3
"""How does the official emulator run pipelines (ExecutePipeline on an enterprise database, #80)?

Usage (from the repository root), with a jar freshly started with `--database-edition enterprise`
under an English locale:
    python3 -I tools/oracle/pipelines.py 127.0.0.1:8088 \\
        > crates/hidane/tests/fixtures/pipelines.json

Seeds a dataset (books with reviews below them, a document holding a value of every type, numbers
of every kind), then runs each case once over REST (`:executePipeline`) and records its outcome:
the results (names relative to the database, fields, whether times are set; the execution time
masked) or the error. The cases are of two kinds:

- stages: each stage the SDKs send (`collection`, `where`, `sort`, `aggregate`, `let`, `union`,
  `update`, …), how they combine, and how malformed stages, arguments and options are refused;
- functions: each function the SDKs know, evaluated by `select` on the document of every type,
  first with arguments that make sense, then with each argument in turn replaced by a value of
  every type and by a missing field.

`crates/hidane/tests/pipelines.rs` replays it. Stdlib only.
"""

import http.client
import json
import sys
import time

HOST = sys.argv[1]
PROJECT = f"pipes-{time.strftime('%H%M%S')}"
DATABASE = f"projects/{PROJECT}/databases/(default)"
OWNER = {"Authorization": "Bearer owner", "Content-Type": "application/json"}
ORACLE = ("cloud-firestore-emulator v1.22.0 "
          "(sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)")


def call(method, path, body=None):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=60)
    c.request(method, path, body=None if body is None else json.dumps(body), headers=OWNER)
    r = c.getresponse()
    text = r.read().decode()
    return r.status, text


# --- values -----------------------------------------------------------------------------------

NULL = {"nullValue": None}


def s(x):
    return {"stringValue": x}


def i(x):
    return {"integerValue": str(x)}


def d(x):
    return {"doubleValue": x}


def b(x):
    return {"booleanValue": x}


def arr(*values):
    return {"arrayValue": {"values": list(values)}}


def mp(**fields):
    return {"mapValue": {"fields": fields}}


def vec(*xs):
    return mp(__type__=s("__vector__"), value=arr(*[d(float(x)) for x in xs]))


def ref(path):
    return {"referenceValue": f"{{database}}/documents/{path}"}


def fr(path):
    return {"fieldReferenceValue": path}


def var(name):
    return {"variableReferenceValue": name}


def fn(name, *args, **options):
    out = {"name": name}
    if args:
        out["args"] = list(args)
    if options:
        out["options"] = options
    return {"functionValue": out}


def stage(name, *args, **options):
    out = {"name": name}
    if args:
        out["args"] = list(args)
    if options:
        out["options"] = options
    return out


def collection(path):
    return stage("collection", {"referenceValue": "/" + path})


def where(condition):
    return stage("where", condition)


def select(**fields):
    return stage("select", mp(**fields))


def sort(*orderings):
    return stage("sort", *[mp(expression=e, direction=s(direction)) for e, direction in orderings])


def limit(n):
    return stage("limit", i(n))


def eq(a, b_):
    return fn("equal", a, b_)


# --- the dataset ------------------------------------------------------------------------------

BOOKS = {
    "b1": dict(title=s("Dune"), author=s("Frank Herbert"), genre=s("Science Fiction"), published=i(1965),
               rating=d(4.6), tags=arr(s("space"), s("politics"), s("desert")),
               awards=mp(hugo=b(True), nebula=b(True)), pages=i(412), embedding=vec(1, 0)),
    "b2": dict(title=s("Neuromancer"), author=s("William Gibson"), genre=s("Science Fiction"), published=i(1984),
               rating=d(4.2), tags=arr(s("cyberpunk"), s("space")), awards=mp(hugo=b(True), nebula=b(True)),
               pages=i(271), embedding=vec(0.5, 0.5)),
    "b3": dict(title=s("Emma"), author=s("Jane Austen"), genre=s("Romance"), published=i(1815), rating=d(3.9),
               tags=arr(s("society"), s("matchmaking")), awards=mp(), pages=i(474), embedding=vec(0, 1)),
    "b4": dict(title=s("The Hobbit"), author=s("J.R.R. Tolkien"), genre=s("Fantasy"), published=i(1937),
               rating=d(4.7), tags=arr(s("adventure"), s("dragons")), awards=mp(hugo=b(False)), pages=i(310),
               embedding=vec(-1, 0)),
    "b5": dict(title=s("Kindred"), author=s("Octavia E. Butler"), genre=s("Science Fiction"), published=i(1979),
               rating=d(4.2), tags=arr(s("time travel")), awards=mp(), pages=i(264), embedding=vec(0.2, 0.9)),
    "b6": dict(title=s("Persuasion"), author=s("Jane Austen"), genre=s("Romance"), published=i(1817),
               rating=d(4.1), tags=arr(), pages=i(249), embedding=vec(0.1, 1)),
    "b7": dict(title=s("Untitled"), author=NULL, genre=s("Fantasy"), published=i(2001), rating=d("NaN"),
               tags=arr(s("adventure")), pages=d(100.5)),
    "b8": dict(title=s("Stub"), genre=s("Poetry"), rating=i(5)),
}
REVIEWS = {
    "b1/reviews/r1": dict(stars=i(5), text=s("classic"), book=ref("books/b1")),
    "b1/reviews/r2": dict(stars=i(3), text=s("long"), book=ref("books/b1")),
    "b4/reviews/r1": dict(stars=i(4), text=s("charming"), book=ref("books/b4")),
}
TYPES = dict(
    n=NULL, t=b(True), f=b(False), i=i(1), i2=i(2), i0=i(0), ineg=i(-3), i10=i(10),
    big=i(9223372036854775807), small=i(-9223372036854775808), d=d(2.5), dneg=d(-2.5), d0=d(0.0),
    nz=d(-0.0), dfrac=d(1234.5678), nan=d("NaN"), inf=d("Infinity"), ninf=d("-Infinity"),
    s=s("abc"), s2=s("Hello World"), e=s(""), u=s("héllo wörld"), sp=s("  pad  "), csv=s("a,b,c"),
    b={"bytesValue": "AQID"}, be={"bytesValue": ""},
    ts={"timestampValue": "2026-01-02T03:04:05.123456Z"}, ts0={"timestampValue": "1970-01-01T00:00:00Z"},
    g={"geoPointValue": {"latitude": 35.6, "longitude": 139.7}},
    g2={"geoPointValue": {"latitude": 34.7, "longitude": 135.5}},
    r=ref("types/x"), r2=ref("books/b1/reviews/r1"),
    a=arr(i(1), s("a"), NULL), ae=arr(), ai=arr(i(3), i(1), i(2)), asx=arr(s("b"), s("a"), s("c")),
    an=arr(i(1), d(2.5), i(3)),
    m=mp(k=i(1), j=s("x")), m2=mp(k=i(2), z=b(True)), me=mp(), mm=mp(inner=mp(k=i(1))),
    v=vec(1, 2), v2=vec(3, 4), v3=vec(1, 2, 3),
)
NUMS = {
    "n1": dict(x=i(1), y=s("a"), k=s("p")),
    "n2": dict(x=i(2), y=s("b"), k=s("p")),
    "n3": dict(x=d(2.5), y=s("a"), k=s("q")),
    "n4": dict(x=NULL, y=s("c"), k=s("q")),
    "n5": dict(x=s("text"), k=s("r")),
    "n6": dict(y=s("d"), k=NULL),
    "n7": dict(x=i(1), y=s("a")),
}
DATASET = ([{"name": f"books/{k}", "fields": v} for k, v in BOOKS.items()]
           + [{"name": f"books/{k}", "fields": v} for k, v in REVIEWS.items()]
           + [{"name": "types/x", "fields": TYPES}]
           + [{"name": f"nums/{k}", "fields": v} for k, v in NUMS.items()])


def fill(value):
    return json.loads(json.dumps(value).replace("{database}", DATABASE))


def seed():
    writes = [{"update": {"name": f"{DATABASE}/documents/{doc['name']}", "fields": fill(doc["fields"])}}
              for doc in DATASET]
    status, text = call("POST", f"/v1/{DATABASE}/documents:commit", {"writes": writes})
    assert status == 200, text


# --- running a case ---------------------------------------------------------------------------

def masked(value):
    """`value` with the times of `current_document()` masked."""
    if isinstance(value, dict):
        return {k: "<time>" if k in ("__create_time__", "__update_time__") else masked(v)
                for k, v in value.items()}
    if isinstance(value, list):
        return [masked(v) for v in value]
    return value


def run(stages, options=None, **request):
    structured = {"pipeline": {"stages": fill(stages)}, **({"options": options} if options else {})}
    body = {"structuredPipeline": structured, **fill(request)}
    status, text = call("POST", f"/v1/{DATABASE}/documents:executePipeline", body)
    text = text.replace(PROJECT, "{project}")
    if status != 200:
        error = json.loads(text)["error"]
        return {"status": error["status"], "message": error.get("message", "")}
    results, timed = [], []
    global last_response
    last_response = {}
    for response in json.loads(text):
        last_response.update(response)
        timed.append("executionTime" in response)
        for doc in response.get("results", []):
            out = {}
            if "name" in doc:
                out["name"] = doc["name"].replace("projects/{project}/databases/(default)/documents/", "")
            out["fields"] = masked(doc.get("fields", {}))
            for key in ("createTime", "updateTime"):
                if key in doc:
                    out[key] = True
            results.append(out)
    return {"status": "OK", "results": results, "execution_time": any(timed)}


cases = []
last_response = {}


ORDERING = ("sort", "find_nearest")


def case(name, stages, options=None, **request):
    # A limit takes the first documents in an order the emulator does not define unless a sort
    # comes before it: sort them by name first.
    ordered = []
    for st in stages:
        if st["name"] in ("limit", "offset") and not any(o["name"] in ORDERING for o in ordered):
            ordered.append(sort((fr("__name__"), "ascending")))
        ordered.append(st)
    stages = ordered
    cases.append({"name": name, "pipeline": stages, **({"options": options} if options else {}),
                  **({"request": request} if request else {}), "outcome": run(stages, options, **request)})


# --- stages -----------------------------------------------------------------------------------

def stage_cases():
    books = collection("books")
    title = fr("title")
    # Sources.
    case("collection", [books])
    case("collection without a leading slash", [stage("collection", {"referenceValue": "books"})])
    case("collection of a subcollection", [collection("books/b1/reviews")])
    case("collection of a document path", [collection("books/b1")])
    case("collection that does not exist", [collection("nothing")])
    case("collection as a string", [stage("collection", s("/books"))])
    case("collection without arguments", [stage("collection")])
    case("collection with two arguments", [stage("collection", {"referenceValue": "/books"}, {"referenceValue": "/nums"})])
    case("collection_group", [stage("collection_group", {"referenceValue": ""}, s("reviews"))])
    case("collection_group of a parent", [stage("collection_group", {"referenceValue": "/books/b1"}, s("reviews"))])
    case("collection_group without a parent", [stage("collection_group", s("reviews"))])
    case("database", [stage("database")])
    case("database, sorted by name", [stage("database"), sort((fr("__name__"), "ascending"))])
    case("documents", [stage("documents", {"referenceValue": "/books/b2"}, {"referenceValue": "/books/b1"})])
    case("documents, one missing", [stage("documents", {"referenceValue": "/books/b1"}, {"referenceValue": "/books/zz"})])
    case("documents of a collection path", [stage("documents", {"referenceValue": "/books"})])
    case("documents twice the same", [stage("documents", {"referenceValue": "/books/b1"}, {"referenceValue": "/books/b1"})])
    case("documents without arguments", [stage("documents")])
    case("no stages", [])
    case("where first", [where(eq(title, s("Dune")))])
    case("two sources", [books, collection("nums")])
    case("unknown stage", [books, stage("frobnicate")])
    # where
    case("where equal", [books, where(eq(fr("genre"), s("Fantasy")))])
    case("where on a missing field", [books, where(eq(fr("nothing"), s("Fantasy")))])
    case("where on null", [books, where(eq(fr("author"), NULL))])
    case("where on NaN", [books, where(eq(fr("rating"), d("NaN")))])
    case("where not a boolean", [books, where(fr("title"))])
    case("where a constant true", [books, where(b(True))])
    case("where a constant false", [books, where(b(False))])
    case("where a constant null", [books, where(NULL)])
    case("where an integer", [books, where(i(1))])
    case("where an error", [books, where(fn("divide", fr("pages"), i(0)))])
    case("where an error on some", [books, where(fn("greater_than", fn("divide", i(100), fn("subtract", fr("published"), i(1965))), i(1)))])
    case("where without arguments", [books, stage("where")])
    case("where with two arguments", [books, stage("where", b(True), b(True))])
    case("where and", [books, where(fn("and", eq(fr("genre"), s("Science Fiction")), fn("greater_than", fr("rating"), d(4.3))))])
    case("where or", [books, where(fn("or", eq(fr("genre"), s("Romance")), eq(fr("genre"), s("Poetry"))))])
    case("where array_contains", [books, where(fn("array_contains", fr("tags"), s("space")))])
    case("where on a nested field", [books, where(eq(fr("awards.hugo"), b(True)))])
    case("where on __name__", [books, where(eq(fr("__name__"), ref("books/b3")))])
    case("where twice", [books, where(eq(fr("genre"), s("Science Fiction"))), where(fn("less_than", fr("published"), i(1980)))])
    # select, add_fields, remove_fields, replace_with
    case("select", [books, select(title=title, year=fr("published"))])
    case("select a missing field", [books, select(title=title, nothing=fr("nothing"))])
    case("select a nested field", [books, select(hugo=fr("awards.hugo"))])
    case("select a dotted alias", [books, select(**{"a.b": title})])
    case("select a constant", [books, limit(1), select(one=i(1))])
    case("select nothing", [books, stage("select", mp())])
    case("select a field reference", [books, stage("select", title)])
    case("select a string", [books, stage("select", s("title"))])
    case("select __name__", [books, limit(2), select(n=fr("__name__"))])
    case("select then where on an alias", [books, select(t=title, y=fr("published")), where(fn("less_than", fr("y"), i(1900)))])
    case("select then where on a dropped field", [books, select(t=title), where(eq(fr("genre"), s("Romance")))])
    case("add_fields", [books, stage("add_fields", mp(decade=fn("subtract", fr("published"), fn("mod", fr("published"), i(10)))))])
    case("add_fields over a field", [books, stage("add_fields", mp(title=fn("to_upper", title)))])
    case("add_fields nested alias", [books, limit(1), stage("add_fields", mp(**{"awards.extra": i(1)}))])
    case("add_fields a missing field", [books, limit(1), stage("add_fields", mp(x=fr("nothing")))])
    case("remove_fields", [books, stage("remove_fields", fr("tags"), fr("awards"), fr("embedding"))])
    case("remove_fields a nested field", [books, stage("remove_fields", fr("awards.hugo"))])
    case("remove_fields a missing field", [books, limit(2), stage("remove_fields", fr("nothing"))])
    case("remove_fields a string", [books, limit(2), stage("remove_fields", s("tags"))])
    case("replace_with a map field", [books, stage("replace_with", fr("awards"), s("full_replace"))])
    case("replace_with a map", [books, limit(2), stage("replace_with", fn("map", s("v"), title), s("full_replace"))])
    case("replace_with merge", [books, limit(2), stage("replace_with", fn("map", s("v"), title), s("merge"))])
    case("replace_with without a mode", [books, limit(2), stage("replace_with", fn("map", s("v"), title))])
    case("replace_with an unknown mode", [books, limit(2), stage("replace_with", fn("map", s("v"), title), s("other"))])
    case("replace_with a string", [books, limit(2), stage("replace_with", title, s("full_replace"))])
    case("replace_with a missing field", [books, limit(2), stage("replace_with", fr("nothing"), s("full_replace"))])
    case("replace_with null", [books, limit(2), stage("replace_with", NULL, s("full_replace"))])
    # sort, limit, offset
    case("sort ascending", [books, sort((fr("published"), "ascending"))])
    case("sort descending", [books, sort((fr("rating"), "descending"), (fr("__name__"), "ascending"))])
    case("sort two keys", [books, sort((fr("genre"), "ascending"), (fr("rating"), "descending"), (fr("__name__"), "descending"))])
    case("sort on a missing field", [books, sort((fr("pages"), "ascending"), (fr("__name__"), "ascending"))])
    case("sort mixed types", [collection("nums"), sort((fr("x"), "ascending"), (fr("__name__"), "ascending"))])
    case("sort mixed types descending", [collection("nums"), sort((fr("x"), "descending"), (fr("__name__"), "descending"))])
    case("sort by an expression", [books, sort((fn("char_length", title), "ascending"), (fr("__name__"), "ascending"))])
    case("sort without a direction", [books, stage("sort", mp(expression=title))])
    case("sort with an unknown direction", [books, stage("sort", mp(expression=title, direction=s("sideways")))])
    case("sort without arguments", [books, stage("sort")])
    case("sort by a field reference", [books, stage("sort", title)])
    case("limit", [books, sort((fr("__name__"), "ascending")), limit(3)])
    case("limit 0", [books, limit(0)])
    case("limit negative", [books, limit(-1)])
    case("limit a double", [books, stage("limit", d(2.0))])
    case("limit a string", [books, stage("limit", s("2"))])
    case("limit huge", [books, sort((fr("__name__"), "ascending")), stage("limit", i(9223372036854775807))])
    case("offset", [books, sort((fr("__name__"), "ascending")), stage("offset", i(5))])
    case("offset negative", [books, stage("offset", i(-1))])
    case("offset past the end", [books, stage("offset", i(50))])
    case("offset then limit", [books, sort((fr("__name__"), "ascending")), stage("offset", i(2)), limit(2)])
    case("limit then offset", [books, sort((fr("__name__"), "ascending")), limit(4), stage("offset", i(2))])
    # aggregate, distinct
    agg = lambda accumulators, groups=None: stage("aggregate", mp(**accumulators), mp(**(groups or {})))
    case("aggregate count", [books, agg({"n": fn("count")})])
    case("aggregate count of a field", [books, agg({"n": fn("count", fr("author"))})])
    case("aggregate count on nothing", [collection("nothing"), agg({"n": fn("count")})])
    case("aggregate sum on nothing", [collection("nothing"), agg({"s": fn("sum", fr("x")), "a": fn("average", fr("x")), "m": fn("maximum", fr("x"))})])
    case("aggregate by genre", [books, agg({"n": fn("count"), "avg": fn("average", fr("rating"))}, {"genre": fr("genre")}), sort((fr("genre"), "ascending"))])
    case("aggregate by two groups", [books, agg({"n": fn("count")}, {"genre": fr("genre"), "author": fr("author")}), sort((fr("genre"), "ascending"), (fr("author"), "ascending"))])
    case("aggregate by an expression", [books, agg({"n": fn("count")}, {"century": fn("divide", fr("published"), i(100))}), sort((fr("century"), "ascending"))])
    case("aggregate by a missing field", [collection("nums"), agg({"n": fn("count")}, {"k": fr("k")}), sort((fr("k"), "ascending"))])
    case("aggregate without accumulators", [books, agg({}, {"genre": fr("genre")}), sort((fr("genre"), "ascending"))])
    case("aggregate not an aggregate", [books, agg({"n": fn("add", i(1), i(2))})])
    case("aggregate a field reference", [books, agg({"n": fr("rating")})])
    case("aggregate with one argument", [books, stage("aggregate", mp(n=fn("count")))])
    case("aggregate then where", [books, agg({"n": fn("count")}, {"genre": fr("genre")}), where(fn("greater_than", fr("n"), i(1))), sort((fr("genre"), "ascending"))])
    for name in ("count", "sum", "average", "minimum", "maximum", "first", "last", "count_distinct", "array_agg",
                 "array_agg_distinct"):
        case(f"aggregate {name} over mixed values", [collection("nums"), sort((fr("__name__"), "ascending")), agg({"r": fn(name, fr("x"))})])
        case(f"aggregate {name} over strings", [collection("nums"), sort((fr("__name__"), "ascending")), agg({"r": fn(name, fr("y"))})])
        case(f"aggregate {name} over numbers", [collection("nums"), where(fn("is_type", fr("x"), s("number"))), sort((fr("__name__"), "ascending")), agg({"r": fn(name, fr("x"))})])
    case("aggregate count_if", [books, agg({"r": fn("count_if", fn("greater_than", fr("rating"), d(4.0)))})])
    case("aggregate count_if not boolean", [books, agg({"r": fn("count_if", fr("rating"))})])
    case("aggregate sum overflow", [collection("types"), stage("union", {"pipelineValue": {"stages": [collection("types")]}}), agg({"r": fn("sum", fr("big"))})])
    case("aggregate sum of doubles", [books, agg({"r": fn("sum", fr("rating"))})])
    case("aggregate sum of integers", [books, agg({"r": fn("sum", fr("published"))})])
    case("aggregate average of integers", [books, where(fn("exists", fr("published"))), agg({"r": fn("average", fr("published"))})])
    case("aggregate maximum of strings", [books, agg({"r": fn("maximum", fr("title")), "q": fn("minimum", fr("title"))})])
    case("aggregate first after sort", [books, sort((fr("published"), "ascending")), agg({"first": fn("first", title), "last": fn("last", title)})])
    case("distinct", [books, stage("distinct", mp(genre=fr("genre"))), sort((fr("genre"), "ascending"))])
    case("distinct two fields", [books, stage("distinct", mp(genre=fr("genre"), author=fr("author"))), sort((fr("genre"), "ascending"), (fr("author"), "ascending"))])
    case("distinct an expression", [books, stage("distinct", mp(lower=fn("to_lower", fr("genre")))), sort((fr("lower"), "ascending"))])
    case("distinct a missing field", [collection("nums"), stage("distinct", mp(k=fr("k"))), sort((fr("k"), "ascending"))])
    case("distinct nothing", [books, stage("distinct", mp())])
    # find_nearest, sample, union, unnest
    for measure in ("cosine", "euclidean", "dot_product"):
        case(f"find_nearest {measure}", [books, stage("find_nearest", fr("embedding"), vec(1, 0.1), s(measure), limit=i(3))])
    case("find_nearest with a distance field", [books, stage("find_nearest", fr("embedding"), vec(1, 0.1), s("euclidean"), limit=i(2), distance_field=fr("dist"))])
    case("find_nearest without a limit", [books, stage("find_nearest", fr("embedding"), vec(1, 0.1), s("euclidean"))])
    case("find_nearest of another dimension", [books, stage("find_nearest", fr("embedding"), vec(1, 0, 0), s("euclidean"), limit=i(3))])
    case("find_nearest an unknown measure", [books, stage("find_nearest", fr("embedding"), vec(1, 0), s("manhattan"), limit=i(3))])
    case("find_nearest an array", [books, stage("find_nearest", fr("embedding"), arr(d(1.0), d(0.0)), s("euclidean"), limit=i(3))])
    case("sample all", [books, stage("sample", i(100), s("documents")), sort((fr("__name__"), "ascending"))])
    case("sample count", [books, stage("sample", i(3), s("documents")), stage("aggregate", mp(n=fn("count")), mp())])
    case("sample 0", [books, stage("sample", i(0), s("documents"))])
    case("sample percent 1", [books, stage("sample", d(1.0), s("percent")), stage("aggregate", mp(n=fn("count")), mp())])
    case("sample percent 0", [books, stage("sample", d(0.0), s("percent"))])
    case("sample negative", [books, stage("sample", i(-1), s("documents"))])
    case("sample percent above 1", [books, stage("sample", d(1.5), s("percent"))])
    case("sample an unknown mode", [books, stage("sample", i(1), s("rows"))])
    case("sample without a mode", [books, stage("sample", i(1))])
    case("union", [collection("nums"), stage("union", {"pipelineValue": {"stages": [collection("books/b4/reviews")]}}), sort((fr("__name__"), "ascending"))])
    case("union with itself", [collection("books/b1/reviews"), stage("union", {"pipelineValue": {"stages": [collection("books/b1/reviews")]}}), sort((fr("__name__"), "ascending"))])
    case("union not a pipeline", [books, stage("union", s("x"))])
    case("union of an empty pipeline", [books, stage("union", {"pipelineValue": {"stages": []}})])
    case("unnest", [books, where(eq(title, s("Dune"))), stage("unnest", fr("tags"), fr("tag")), select(tag=fr("tag"))])
    case("unnest with an index", [books, where(eq(title, s("Dune"))), stage("unnest", fr("tags"), fr("tag"), index_field=fr("i")), select(tag=fr("tag"), i=fr("i"))])
    case("unnest an empty array", [books, where(eq(title, s("Persuasion"))), stage("unnest", fr("tags"), fr("tag"))])
    case("unnest a missing field", [books, where(eq(title, s("Stub"))), stage("unnest", fr("tags"), fr("tag"))])
    case("unnest a string", [books, where(eq(title, s("Dune"))), stage("unnest", fr("title"), fr("t"))])
    case("unnest over the field", [books, where(eq(title, s("Dune"))), stage("unnest", fr("tags"), fr("tags")), select(tags=fr("tags"))])
    case("unnest a constant array", [books, where(eq(title, s("Dune"))), stage("unnest", fn("array", i(1), i(2)), fr("x")), select(x=fr("x"))])
    case("unnest an alias string", [books, where(eq(title, s("Dune"))), stage("unnest", fr("tags"), s("tag"))])
    # let and subpipelines
    case("let a constant", [books, limit(1), stage("let", mp(x=s("v"))), select(x=var("x"))])
    case("let a field", [books, where(eq(title, s("Emma"))), stage("let", mp(y=fr("published"))), select(y=var("y"))])
    case("an unknown variable", [books, select(x=var("nothing"))])
    case("let twice the same", [books, limit(1), stage("let", mp(x=i(1))), stage("let", mp(x=i(2))), select(x=var("x"))])
    case("a scalar subpipeline", [books, where(eq(title, s("Dune"))), stage("let", mp(id=fr("__name__"))),
                                  select(stars=fn("scalar", {"pipelineValue": {"stages": [
                                      stage("collection_group", {"referenceValue": ""}, s("reviews")),
                                      where(eq(fr("book"), var("id"))),
                                      stage("aggregate", mp(s=fn("sum", fr("stars"))), mp())]}}))])
    case("a scalar subpipeline of several results", [books, limit(1), select(x=fn("scalar", {"pipelineValue": {"stages": [collection("nums")]}}))])
    case("a scalar subpipeline of no result", [books, limit(1), select(x=fn("scalar", {"pipelineValue": {"stages": [collection("nothing")]}}))])
    case("an array subpipeline", [books, limit(1), select(x=fn("array", {"pipelineValue": {"stages": [collection("books/b1/reviews"), sort((fr("stars"), "ascending")), select(s=fr("stars"))]}}))])
    case("current_document", [books, where(eq(title, s("Stub"))), select(doc=fn("current_document"))])
    reviews = lambda *more: {"pipelineValue": {"stages": [stage("subcollection", s("reviews")), *more]}}
    case("subcollection in an array subpipeline", [books, where(eq(title, s("Dune"))), stage("add_fields", mp(
        r=fn("array", reviews(sort((fr("stars"), "ascending")), select(s=fr("stars")))))), select(t=title, r=fr("r"))])
    case("subcollection that has no documents", [books, where(eq(title, s("Kindred"))), select(
        r=fn("array", reviews(select(s=fr("stars")))))])
    case("subcollection in a scalar subpipeline", [books, sort((fr("__name__"), "ascending")), select(
        n=fn("scalar", reviews(stage("aggregate", mp(n=fn("count")), mp()))))])
    case("subcollection without a document", [books, where(eq(title, s("Dune"))), select(t=title), select(
        r=fn("array", reviews(select(s=fr("stars")))))])
    case("subcollection first", [stage("subcollection", s("reviews"))])
    case("subcollection of a path", [books, where(eq(title, s("Dune"))), select(
        r=fn("array", {"pipelineValue": {"stages": [stage("subcollection", s("reviews/r1/x"))]}}))])
    case("subcollection of a reference", [books, where(eq(title, s("Dune"))), select(
        r=fn("array", {"pipelineValue": {"stages": [stage("subcollection", {"referenceValue": "/reviews"})]}}))])
    case("subcollection without arguments", [books, where(eq(title, s("Dune"))), select(
        r=fn("array", {"pipelineValue": {"stages": [stage("subcollection")]}}))])
    case("select __name__ as itself", [books, where(eq(title, s("Dune"))), select(t=title, __name__=fr("__name__"))])
    case("select __name__ as itself and sort", [books, select(t=title, __name__=fr("__name__")),
                                                 sort((fr("__name__"), "descending")), limit(2)])
    case("array_filter", [books, where(eq(title, s("Dune"))), select(x=fn("array_filter", fr("tags"), s("t"), fn("not_equal", var("t"), s("space"))))])
    case("array_filter not a boolean", [books, where(eq(title, s("Dune"))), select(x=fn("array_filter", fr("tags"), s("t"), var("t")))])
    case("array_transform", [books, where(eq(title, s("Dune"))), select(x=fn("array_transform", fr("tags"), s("t"), fn("char_length", var("t"))))])
    case("array_transform with an index", [books, where(eq(title, s("Dune"))), select(x=fn("array_transform", fr("tags"), s("t"), s("n"), fn("add", fn("char_length", var("t")), var("n"))))])
    case("array_filter an alias that is a field", [books, where(eq(title, s("Dune"))), select(x=fn("array_filter", fr("tags"), fr("t"), b(True)))])
    # update and delete
    case("update", [stage("documents", {"referenceValue": "/nums/n1"}), stage("update", mp(z=i(9)))])
    case("update then read", [stage("documents", {"referenceValue": "/nums/n1"})])
    case("update with an expression", [collection("nums"), where(eq(fr("k"), s("p"))), stage("update", mp(x=fn("add", fr("x"), i(10))))])
    case("update a missing document", [stage("documents", {"referenceValue": "/nums/zz"}), stage("update", mp(z=i(9)))])
    case("update after select", [collection("nums"), select(x=fr("x")), stage("update", mp(z=i(1)))])
    case("update without arguments", [stage("documents", {"referenceValue": "/nums/n2"}), stage("update")])
    case("update twice", [stage("documents", {"referenceValue": "/nums/n2"}), stage("update", mp(z=i(1))), stage("update", mp(z=i(2)))])
    case("update after add_fields", [stage("documents", {"referenceValue": "/nums/n3"}),
                                     stage("add_fields", mp(extra=fr("__name__"))), stage("update", mp(z=i(2)))])
    case("update after remove_fields", [stage("documents", {"referenceValue": "/nums/n3"}),
                                        stage("remove_fields", fr("y")), stage("update", mp(z=i(3)))])
    case("update after add_fields and remove_fields, read", [stage("documents", {"referenceValue": "/nums/n3"})])
    case("delete", [stage("documents", {"referenceValue": "/nums/n7"}), stage("delete")])
    case("delete then read", [stage("documents", {"referenceValue": "/nums/n7"})])
    case("delete a missing document", [stage("documents", {"referenceValue": "/nums/zz"}), stage("delete")])
    case("delete with an argument", [stage("documents", {"referenceValue": "/nums/n6"}), stage("delete", i(1))])
    case("delete then where", [stage("documents", {"referenceValue": "/nums/n6"}), stage("delete"), where(b(True))])
    case("search", [books, stage("search", query=fn("document_matches", s("dune")))])
    # Each stage's signature, as its errors print it: no arguments, one more, an unknown option.
    vector = vec(1, 0)
    valid = {
        "collection": [{"referenceValue": "/books"}],
        "collection_group": [{"referenceValue": ""}, s("reviews")],
        "database": [],
        "documents": [{"referenceValue": "/books/b1"}],
        "where": [b(True)],
        "select": [mp(t=title)],
        "add_fields": [mp(t=title)],
        "remove_fields": [title],
        "replace_with": [fr("awards"), s("full_replace")],
        "sort": [mp(expression=title, direction=s("ascending"))],
        "limit": [i(1)],
        "offset": [i(1)],
        "aggregate": [mp(n=fn("count")), mp()],
        "distinct": [mp(g=fr("genre"))],
        "find_nearest": [fr("embedding"), vector, s("euclidean")],
        "sample": [i(1), s("documents")],
        "union": [{"pipelineValue": {"stages": [collection("nums")]}}],
        "unnest": [fr("tags"), fr("tag")],
        "let": [mp(x=i(1))],
        "update": [mp(z=i(1))],
        "delete": [],
    }
    for name, args in valid.items():
        # Updates and deletes run on a collection without documents, which they leave alone.
        source = ([] if name in ("collection", "collection_group", "database", "documents")
                  else [collection("nothing")] if name in ("update", "delete") else [books])
        case(f"{name} without arguments", source + [stage(name)])
        case(f"{name} with one argument more", source + [stage(name, *args, i(1))])
        case(f"{name} with an unknown option", source + [stage(name, *args, frobnicate=b(True))])
    case("replace_with merge_overwrite_existing", [books, stage("replace_with", fn("map", s("title"), s("X"), s("new"), i(1)), s("merge_overwrite_existing"))])
    case("replace_with merge_keep_existing", [books, stage("replace_with", fn("map", s("title"), s("X"), s("new"), i(1)), s("merge_keep_existing"))])
    case("replace_with merge of a string", [books, stage("replace_with", title, s("merge_keep_existing"))])
    case("unnest keeps the fields", [collection("books/b4/reviews"), stage("unnest", fn("array", i(1), i(2)), fr("x"))])
    case("unnest a map", [books, where(eq(title, s("Dune"))), stage("unnest", fr("awards"), fr("a"))])
    case("unnest null", [books, where(eq(title, s("Untitled"))), stage("unnest", fr("author"), fr("a"))])
    case("add_fields under a string", [books, where(eq(title, s("Dune"))), stage("add_fields", mp(**{"title.x": i(1)}))])
    case("select __name__ as an alias", [books, where(eq(title, s("Dune"))), select(__name__=title)])
    case("select the times", [books, where(eq(title, s("Dune"))), select(
        c=fn("type", fr("__create_time__")), same=eq(fr("__create_time__"), fr("__update_time__")))])
    case("where a boolean field", [books, where(fr("awards.hugo"))])
    case("sort by __name__ descending", [collection("books/b1/reviews"), sort((fr("__name__"), "descending"))])
    case("sort by a map", [books, sort((fr("awards"), "ascending"), (fr("__name__"), "ascending")), select(a=fr("awards"))])
    case("aggregate a count with an argument and a group", [collection("nums"), stage("aggregate", mp(n=fn("count", fr("x"))), mp(k=fr("k"))), sort((fr("k"), "ascending"))])
    case("aggregate an alias in both", [books, stage("aggregate", mp(genre=fn("count")), mp(genre=fr("genre")))])
    case("aggregate a nested alias", [books, stage("aggregate", mp(**{"a.b": fn("count")}), mp())])
    # Options and the request.
    case("explain_options", [books, limit(1)], options={"explain_options": mp(mode=s("execute"))})
    case("an unknown stage option", [books, stage("limit", i(1), frobnicate=b(True))])
    case("an unknown function", [books, select(x=fn("frobnicate", i(1)))])
    case("a function without a name", [books, select(x={"functionValue": {"args": [i(1)]}})])
    case("an unknown function option", [books, limit(1), select(x=fn("add", i(1), i(2), frobnicate=b(True)))])


# --- functions --------------------------------------------------------------------------------

# Every argument is a field of `types/x` (by name) or a value (a dict); `missing` is no field.
SWEEP = ["n", "t", "i", "d", "nan", "s", "e", "b", "ts", "g", "r", "a", "m", "v", "missing"]


def arg(x):
    return fr(x) if isinstance(x, str) else x


# name: argument lists that make sense; the first is swept, argument by argument.
FUNCTIONS = {
    "equal": [("i", "i2"), ("i", "i"), ("i", "d"), ("n", "n"), ("nan", "nan"), ("a", "a"), ("m", "m"),
              ("v", "v"), ("missing", "missing"), ("d0", "nz"), ("r", "r")],
    "not_equal": [("i", "i2"), ("i", "i"), ("n", "n"), ("nan", "nan"), ("missing", "missing")],
    "less_than": [("i", "i2"), ("i2", "i"), ("i", "d"), ("s", "s2"), ("n", "n"), ("nan", "nan"), ("nan", "i"),
                  ("ts0", "ts"), ("a", "ai"), ("i", "s"), ("ninf", "nan")],
    "less_than_or_equal": [("i", "i2"), ("i", "i"), ("n", "n"), ("nan", "nan"), ("missing", "missing")],
    "greater_than": [("i", "i2"), ("i2", "i"), ("n", "n"), ("nan", "nan"), ("i", "s")],
    "greater_than_or_equal": [("i", "i2"), ("i", "i"), ("n", "n"), ("nan", "nan")],
    "equal_any": [("i", "ai"), ("i", "asx"), ("n", "a"), ("nan", arr(d("NaN"))), ("i", "ae"), ("missing", "ai")],
    "not_equal_any": [("i", "ai"), ("i", "asx"), ("n", "a"), ("i", "ae")],
    "and": [("t", "f"), ("t", "t"), ("f", "f"), ("t", "n"), ("f", "n"), ("n", "f"), ("t", "missing"),
            ("t", "t", "f"), ("t", "i")],
    "or": [("t", "f"), ("f", "f"), ("t", "n"), ("f", "n"), ("n", "t"), ("f", "missing"), ("f", "f", "t")],
    "xor": [("t", "f"), ("t", "t"), ("f", "f"), ("t", "n"), ("t", "t", "t")],
    "nor": [("t", "f"), ("f", "f"), ("f", "n"), ("t", "n")],
    "not": [("t",), ("f",), ("n",)],
    "exists": [("i",), ("n",), ("missing",)],
    "is_absent": [("i",), ("missing",)],
    "is_error": [("i",), (fn("divide", fr("i"), fr("i0")),)],
    "is_type": [("s", s("string")), ("i", s("int64")), ("d", s("float64")), ("i", s("number")),
                ("n", s("null")), ("v", s("vector")), ("m", s("map")), ("a", s("array")), ("ts", s("timestamp")),
                ("g", s("geo_point")), ("r", s("reference")), ("b", s("bytes")), ("t", s("boolean")),
                ("i", s("int32")), ("s", s("frobnicate")), ("s", "s")],
    "type": [("i",)],
    "add": [("i", "i2"), ("i", "d"), ("big", "i"), ("d", "nan"), ("i", "inf"), ("inf", "ninf"), ("i", "i", "i"),
            ("d", "dneg")],
    "subtract": [("i", "i2"), ("i", "d"), ("small", "i"), ("inf", "inf")],
    "multiply": [("i2", "i10"), ("i", "d"), ("big", "i2"), ("inf", "i0"), ("i2", "i2", "i2")],
    "divide": [("i10", "i2"), ("i", "i2"), ("i", "d"), ("i", "i0"), ("d", "d0"), ("d0", "d0"), ("dneg", "d0"),
               ("small", {"integerValue": "-1"}), ("ineg", "i2")],
    "mod": [("i10", {"integerValue": "3"}), ("ineg", "i2"), ("i", "i0"), ("d", "i2"), ("d", "d0"), ("i10", d(2.5))],
    "pow": [("i2", "i10"), ("i2", d(0.5)), ("i2", "ineg"), ("i0", "ineg"), ("ineg", d(0.5)), ("big", "i2")],
    "abs": [("ineg",), ("dneg",), ("small",), ("nz",)],
    "ceil": [("d",), ("dneg",), ("i",), ("nan",)],
    "floor": [("d",), ("dneg",), ("i",), ("inf",)],
    "exp": [("i",), ("i0",), ("d",), ("big",)],
    "ln": [("i",), ("i0",), ("ineg",), ("d",)],
    "log10": [("i10",), ("i0",), ("ineg",), ("d",)],
    "sqrt": [("i2",), ("ineg",), ("d",), ("i0",)],
    "round": [("d",), ("dneg",), ("dfrac", "i2"), ("dfrac", {"integerValue": "-2"}), ("i",), ("i10", {"integerValue": "-1"}),
              (d(0.5),), (d(1.5),), (d(-0.5),)],
    "trunc": [("dfrac",), ("dneg",), ("dfrac", "i2"), ("dfrac", {"integerValue": "-2"}), ("i",)],
    "char_length": [("s",), ("u",), ("e",)],
    "byte_length": [("s",), ("u",), ("b",), ("be",)],
    "to_lower": [("s2",), ("u",)],
    "to_upper": [("s2",), ("u",)],
    "trim": [("sp",), ("s",), ("csv", s("a,c")), ("b", {"bytesValue": "AQ=="})],
    "ltrim": [("sp",), ("csv", s("a,"))],
    "rtrim": [("sp",), ("csv", s("c,"))],
    "reverse": [("s",), ("u",), ("ai",), ("b",)],
    "string_reverse": [("s",), ("u",)],
    "substring": [("s2", "i2"), ("s2", "i", "i2"), ("s2", "ineg"), ("s2", "i10", "i2"), ("u", "i", "i2"),
                  ("s2", "i", "ineg"), ("b", "i")],
    "string_concat": [("s", "s2"), ("s", "e", "u"), ("s",)],
    "concat": [("s", "s2"), ("ai", "asx"), ("b", "b"), ("s", "ai")],
    "starts_with": [("s2", s("Hell")), ("s2", s("World")), ("s2", "e"), ("b", {"bytesValue": "AQ=="})],
    "ends_with": [("s2", s("World")), ("s2", s("Hell"))],
    "string_contains": [("s2", s("lo W")), ("s2", s("xyz"))],
    "like": [("s2", s("Hello%")), ("s2", s("_ello World")), ("s2", s("hello%")), ("s2", s("%o W%"))],
    "regex_contains": [("s2", s("o W")), ("s2", s("^World")), ("s2", s("(")), ("s2", s("(?i)hello"))],
    "regex_match": [("s2", s("H.*d")), ("s2", s("ello")), ("s2", s("["))],
    "regex_find": [("s2", s("o.")), ("s2", s("z")), ("s2", s("(l+)"))],
    "regex_find_all": [("s2", s("o.")), ("s2", s("z")), ("s2", s("(l)(o)"))],
    "split": [("csv", s(",")), ("csv", "e"), ("s", s("x")), ("b", {"bytesValue": "Ag=="})],
    "join": [("asx", s("-")), ("a", s(",")), ("ae", s(",")), ("ai", s(","))],
    "string_index_of": [("s2", s("o")), ("s2", s("z")), ("u", s("w")), ("s2", "e")],
    "string_repeat": [("s", "i2"), ("s", "i0"), ("s", "ineg")],
    "string_replace_all": [("s2", s("o"), s("0")), ("s2", s("z"), s("0")), ("s2", "e", s("-"))],
    "string_replace_one": [("s2", s("o"), s("0")), ("s2", s("z"), s("0"))],
    "length": [("s",), ("u",), ("ai",), ("b",), ("m",), ("v",)],
    "array": [("i", "s"), (), ("missing",), ("n",)],
    "array_concat": [("ai", "asx"), ("ai",), ("ai", "ae", "a")],
    "array_contains": [("ai", "i"), ("a", "n"), ("ai", "s"), ("ae", "i")],
    "array_contains_all": [("ai", fn("array", fr("i"), fr("i2"))), ("ai", "asx"), ("ai", "ae")],
    "array_contains_any": [("ai", fn("array", fr("i"), fr("s"))), ("ai", "asx"), ("ai", "ae")],
    "array_length": [("ai",), ("ae",)],
    "array_reverse": [("ai",), ("ae",)],
    "array_first": [("ai",), ("ae",)],
    "array_first_n": [("ai", "i2"), ("ai", "i0"), ("ai", "i10"), ("ai", "ineg")],
    "array_last": [("ai",), ("ae",)],
    "array_last_n": [("ai", "i2"), ("ai", "i10"), ("ai", "ineg")],
    "array_get": [("ai", "i"), ("ai", "ineg"), ("ai", "i10"), ("ai", "d")],
    "array_index_of": [("ai", "i2", s("first")), ("ai", "i10", s("first")), ("a", "n", s("last")),
                       ("ai", "i2", s("middle"))],
    "array_index_of_all": [("ai", "i2"), (fn("array", fr("i"), fr("i2"), fr("i")), "i"), ("ai", "i10")],
    "array_slice": [("ai", "i"), ("ai", "i", "i"), ("ai", "ineg"), ("ai", "i", "ineg"), ("ai", "i10")],
    "maximum": [("i", "i2"), ("i", "s"), ("i", "n"), ("n", "n"), ("i", "missing"), ("i", "nan"), ("i", "d", "i2")],
    "minimum": [("i", "i2"), ("i", "s"), ("i", "n"), ("i", "nan")],
    "maximum_n": [("ai", "i2"), ("an", "i2"), ("ai", "i0"), ("ai", "ineg")],
    "minimum_n": [("ai", "i2"), ("asx", "i2"), ("ai", "i10")],
    "map": [(s("k"), "i"), (), (s("k"), "missing"), (s("k"),), ("i", "i"), (s("a"), "i", s("a"), "i2")],
    "map_get": [("m", s("k")), ("m", s("nothing")), ("mm", s("inner")), ("m", "s")],
    "map_set": [("m", s("k"), "i2"), ("m", s("new"), "s"), ("m", s("k"), "missing"), ("m", s("a"), "i", s("b"), "i2")],
    "map_remove": [("m", s("k")), ("m", s("nothing"))],
    "map_merge": [("m", "m2"), ("m", "me"), ("m", "m2", "mm")],
    "map_keys": [("m",), ("me",)],
    "map_values": [("m",), ("me",)],
    "map_entries": [("m",), ("me",)],
    "get_field": [("m", s("k")), ("mm", s("inner.k")), ("m", s("nothing"))],
    "timestamp_add": [("ts", s("day"), "i"), ("ts", s("microsecond"), "i10"), ("ts", s("hour"), "ineg"),
                      ("ts", s("week"), "i"), ("ts", s("month"), "i"), ("ts", s("fortnight"), "i"),
                      ("ts", s("day"), "big")],
    "timestamp_subtract": [("ts", s("day"), "i"), ("ts", s("second"), "i10"), ("ts0", s("day"), "i")],
    "timestamp_diff": [("ts", "ts0", s("day")), ("ts0", "ts", s("second")), ("ts", "ts", s("microsecond")),
                       ("ts", "ts0", s("month"))],
    "timestamp_extract": [("ts", s("year")), ("ts", s("month")), ("ts", s("day")), ("ts", s("hour")),
                          ("ts", s("minute")), ("ts", s("second")), ("ts", s("millisecond")),
                          ("ts", s("microsecond")), ("ts", s("dayofweek")), ("ts", s("dayofyear")),
                          ("ts", s("week")), ("ts", s("isoweek")), ("ts", s("isoyear")), ("ts", s("quarter")),
                          ("ts", s("hour"), s("Asia/Tokyo")), ("ts", s("hour"), s("+05:30")),
                          ("ts", s("hour"), s("Mars/Olympus")), ("ts", s("century"))],
    "timestamp_trunc": [("ts", s("day")), ("ts", s("hour")), ("ts", s("month")), ("ts", s("year")),
                        ("ts", s("week")), ("ts", s("isoweek")), ("ts", s("quarter")), ("ts", s("minute")),
                        ("ts", s("second")), ("ts", s("millisecond")), ("ts", s("microsecond")),
                        ("ts", s("day"), s("Asia/Tokyo")), ("ts", s("week(monday)")), ("ts", s("decade"))],
    "timestamp_to_unix_seconds": [("ts",), ("ts0",)],
    "timestamp_to_unix_millis": [("ts",)],
    "timestamp_to_unix_micros": [("ts",)],
    "unix_seconds_to_timestamp": [("i10",), ("ineg",), ("big",)],
    "unix_millis_to_timestamp": [("i10",), ("big",)],
    "unix_micros_to_timestamp": [("i10",), ("big",)],
    "cosine_distance": [("v", "v2"), ("v", "v"), ("v", "v3"), ("v", fn("array", fr("d"), fr("d")))],
    "dot_product": [("v", "v2"), ("v", "v3")],
    "euclidean_distance": [("v", "v2"), ("v", "v3")],
    "vector_length": [("v",), ("v3",)],
    "conditional": [("t", "i", "s"), ("f", "i", "s"), ("n", "i", "s"), ("i", "i", "s")],
    "switch_on": [("f", "i", "t", "s"), ("f", "i", "f", "s", "d"), ("f", "i"), ("n", "i", "d")],
    "coalesce": [("n", "i"), ("missing", "n", "s"), ("n", "n"), ("i", "s")],
    "if_null": [("n", "i"), ("i", "s"), ("missing", "s")],
    "if_absent": [("missing", "i"), ("n", "i"), ("i", "s")],
    "if_error": [(fn("divide", fr("i"), fr("i0")), "s"), ("i", "s")],
    "document_id": [("r",), ("r2",), ("s",)],
    "collection_id": [("r",), ("r2",)],
    "parent": [("r",), ("r2",)],
    "geo_distance": [("g", "g2")],
    "current_document": [()],
    "document_matches": [(s("x"),)],
    "score": [()],
    "between": [("i", "i0", "i2")],
    # The aggregate functions in a scalar expression: `sum` adds an array's elements.
    "sum": [("ai",), ("an",), ("ae",), ("a",), (fn("array", fr("big"), fr("i")),),
            (fn("array", fr("d"), fr("nan")),), (fn("array", fr("i"), fr("s")),)],
    "average": [("ai",)],
    "count": [("ai",)],
    "first": [("ai",)],
    "last": [("ai",)],
}
NONDETERMINISTIC = {
    "rand": [(fn("type", fn("rand")),), (fn("and", fn("greater_than_or_equal", fn("rand"), d(0.0)),
                                              fn("less_than", fn("rand"), d(1.0))),)],
    "current_timestamp": [(fn("type", fn("current_timestamp")),)],
}


def function_cases():
    x = [collection("types")]
    for name, valid in FUNCTIONS.items():
        seen = set()

        def one(label, args):
            key = json.dumps(args, sort_keys=True)
            if key in seen:
                return
            seen.add(key)
            case(f"{name}: {label}", x + [select(r=fn(name, *[arg(a) for a in args]))])

        for args in valid:
            one(", ".join(a if isinstance(a, str) else json.dumps(a, separators=(",", ":")) for a in args), list(args))
        first = list(valid[0])
        for position in range(min(len(first), 3)):
            for sample in SWEEP:
                args = first[:position] + [sample] + first[position + 1:]
                one(f"argument {position + 1} {sample}", args)
        if first:
            one("no arguments", [])
            one("one argument more", first + ["i"])
    for name, valid in NONDETERMINISTIC.items():
        for n, (expression,) in enumerate(valid):
            case(f"{name}: {n + 1}", x + [select(r=expression)])
        case(f"{name}: an argument", x + [select(r=fn(name, fr("i")))])


# --- requests: transactions, read times, malformed requests ---------------------------------

sequences = []


def sequence(name, steps):
    """Runs `steps` in order, each a REST call: `begin` (a transaction, whose ID later steps
    name as `{transaction}`), `write` (a commit; later steps name the first one's time as
    `{time}`), `pipeline` (`:executePipeline`; a transaction it begins is the one `commit`
    commits), `raw` (`:executePipeline` with this body), `commit` (the transaction) or `get` (a
    document)."""
    transaction, at, answers = None, None, []
    for verb, arg in steps:
        if verb == "begin":
            status, text = call("POST", f"/v1/{DATABASE}/documents:beginTransaction", {"options": arg})
            transaction = json.loads(text)["transaction"]
            answers.append({"status": status})
        elif verb == "write":
            writes = [{"update": {"name": f"{DATABASE}/documents/{path}", "fields": fields}}
                      for path, fields in arg.items()]
            status, text = call("POST", f"/v1/{DATABASE}/documents:commit", {"writes": writes})
            assert status == 200, text
            at = at or json.loads(text)["commitTime"]
            answers.append({"status": status})
        elif verb == "pipeline":
            stages, request = arg
            request = json.loads(json.dumps(request).replace("{transaction}", transaction or "")
                                 .replace("{time}", at or ""))
            outcome = run(stages, **request)
            if outcome["status"] == "OK":
                outcome["transaction"] = "transaction" in last_response
                transaction = last_response.get("transaction", transaction)
            answers.append(outcome)
        elif verb == "raw":
            status, text = call("POST", f"/v1/{DATABASE}/documents:executePipeline", arg)
            error = json.loads(text).get("error", {}) if status != 200 else {}
            answers.append({"status": error.get("status", "OK"), "message": error.get("message", "")})
        elif verb == "commit":
            status, text = call("POST", f"/v1/{DATABASE}/documents:commit", {"transaction": transaction})
            answers.append({"status": status})
        elif verb == "get":
            status, text = call("GET", f"/v1/{DATABASE}/documents/{arg}")
            answers.append({"status": status, "fields": json.loads(text).get("fields") if status == 200 else None})
    sequences.append({"name": name, "steps": steps, "answers": answers})


def request_cases():
    tx = {"tx/a": {"v": i(1)}, "tx/b": {"v": i(2)}, "tx/c": {"v": i(3)}}
    sequence("a delete in a transaction", [
        ("write", tx), ("begin", {"readWrite": {}}),
        ("pipeline", ([stage("documents", {"referenceValue": "/tx/a"}), stage("delete")], {"transaction": "{transaction}"})),
        ("get", "tx/a"), ("commit", None), ("get", "tx/a")])
    sequence("an update in a transaction", [
        ("begin", {"readWrite": {}}),
        ("pipeline", ([stage("documents", {"referenceValue": "/tx/b"}), stage("update", mp(v=i(20)))], {"transaction": "{transaction}"})),
        ("get", "tx/b"), ("commit", None), ("get", "tx/b")])
    sequence("a read in a transaction", [
        ("begin", {"readWrite": {}}),
        ("pipeline", ([collection("tx"), sort((fr("__name__"), "ascending"))], {"transaction": "{transaction}"})),
        ("commit", None)])
    sequence("a read in a new read-only transaction", [
        ("pipeline", ([collection("tx"), sort((fr("__name__"), "ascending"))], {"newTransaction": {"readOnly": {}}}))])
    sequence("a read at a read time", [
        ("write", {"tx/c": {"v": i(30)}}), ("write", {"tx/c": {"v": i(300)}}),
        ("pipeline", ([stage("documents", {"referenceValue": "/tx/c"})], {"readTime": "{time}"})),
        ("pipeline", ([stage("documents", {"referenceValue": "/tx/c"})], {}))])
    sequence("requests without a pipeline", [
        ("raw", {}), ("raw", {"structuredPipeline": {}}), ("raw", {"structuredPipeline": {"pipeline": {}}})])
    sequence("an update in a new transaction", [
        ("write", {"tx2/a": {"v": i(1)}}),
        ("pipeline", ([stage("documents", {"referenceValue": "/tx2/a"}), stage("update", mp(w=i(1)))], {"newTransaction": {"readWrite": {}}})),
        ("get", "tx2/a"), ("commit", None), ("get", "tx2/a")])
    sequence("a read in a new read-write transaction", [
        ("write", {"tx3/a": {"v": i(1)}}),
        ("pipeline", ([collection("tx3")], {"newTransaction": {"readWrite": {}}})), ("commit", None)])


def main():
    seed()
    stage_cases()
    function_cases()
    request_cases()
    json.dump({
        "oracle": ORACLE,
        "generated_by": "tools/oracle/pipelines.py",
        "dataset": DATASET,
        "cases": cases,
        "sequences": sequences,
    }, sys.stdout, ensure_ascii=False, indent=None, separators=(",", ":"))
    print()


main()
