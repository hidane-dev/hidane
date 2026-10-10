"""How does the official emulator read the `Authorization` header? (#24)

Usage (from the repository root, `grpcurl` on PATH):
    python3 -I tools/oracle/auth.py 127.0.0.1:8086 > crates/hidane/tests/fixtures/auth.json

Records, without Security Rules loaded:

- `classify`: which header values make an administrator (`:listCollectionIds` answers 200),
  a user or anonymous caller (403), or fail (`invalid jwt`, or `UNKNOWN` without a message),
  over REST. Unsigned JWTs are read without verification: any `alg`, any claims, no
  expiry or audience check, but each segment must be URL-safe base64 of a JSON object.
- `grpc`: when the check runs. Each RPC first parses the resource it names (database,
  document, parent), then reads the header, then validates the rest of the request, and only
  then requires an administrator. Listen and Write read the header when the stream opens.
- `rest`: the same through REST and the emulator's own endpoints (`/emulator/v1/…` reads the
  header too, `/` and `/reset` do not).

`crates/hidane/tests/auth.rs` replays every case against hidane.
"""

import base64
import http.client
import json
import re
import subprocess
import sys

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
DB = "projects/auth/databases/(default)"
DOCS = f"/v1/{DB}/documents"


def seg(text):
    return base64.urlsafe_b64encode(text.encode()).decode().rstrip("=")


def jwt(payload, header='{"alg":"none","type":"JWT"}', signature=""):
    return f"Bearer {seg(header)}.{seg(payload)}.{signature}"


# What firebase-js-sdk's createMockUserToken sends (fixed times: nothing is verified).
MOCK = json.dumps({
    "iss": "https://securetoken.google.com/auth", "aud": "auth", "iat": 1700000000,
    "exp": 1700003600, "auth_time": 1700000000, "sub": "alice", "user_id": "alice",
    "firebase": {"sign_in_provider": "custom", "identities": {}},
}, separators=(",", ":"))

CLASSIFY = [
    ("no header", None),
    ("owner", "Bearer owner"),
    ("lower-case scheme", "bearer owner"),
    ("upper-case scheme", "BEARER owner"),
    ("Owner", "Bearer Owner"),
    ("OWNER", "Bearer OWNER"),
    ("two spaces", "Bearer  owner"),
    ("tab", "Bearer\towner"),
    ("owner and more", "Bearer ownerx"),
    ("no scheme", "owner"),
    ("no space", "Bearerowner"),
    ("scheme only", "Bearer"),
    ("basic", "Basic dXNlcjpwYXNz"),
    ("ya29. alone", "Bearer ya29."),
    ("ya29.", "Bearer ya29.a0AfH6SM"),
    ("Ya29.", "Bearer Ya29.x"),
    ("YA29.", "Bearer YA29.x"),
    ("ya29 without a dot", "Bearer ya29"),
    ("ya29 then a letter", "Bearer ya29x"),
    ("garbage", "Bearer garbage"),
    ("mock user token", jwt(MOCK)),
    ("empty header and claims", "Bearer e30.e30."),
    ("claims without sub", jwt('{"iss":"x","aud":"auth"}')),
    ("expired", jwt('{"sub":"alice","exp":1}')),
    ("other audience", jwt('{"sub":"alice","aud":"other","iss":"https://securetoken.google.com/other"}')),
    ("RS256 with a signature", jwt(MOCK, '{"alg":"RS256","typ":"JWT","kid":"x"}', "c2ln")),
    ("alg not a string", jwt("{}", '{"alg":1}')),
    ("sub a number", jwt('{"sub":1}')),
    ("claims with trailing space", jwt("{} ")),
    ("duplicate claims", jwt('{"a":1,"a":2}')),
    ("padded segments", "Bearer e30=.e30=."),
    ("over-padded segment", "Bearer e30.e30===."),
    ("URL-safe alphabet", jwt('{"sub":"a?>~~~"}')),
    ("standard alphabet", "Bearer e30." + base64.b64encode(b'{"sub":"a?>~~~"}').decode().rstrip("=") + "."),
    ("signature of 2 characters", "Bearer e30.e30.ab"),
    ("signature of 3 characters", "Bearer e30.e30.abc"),
    ("signature with - and _", "Bearer e30.e30._-_-"),
    ("signature of 1 character", "Bearer e30.e30.a"),
    ("signature of 1 character padded", "Bearer e30.e30.a==="),
    ("signature of 5 characters", "Bearer e30.e30.abc-_"),
    ("signature not base64", "Bearer e30.e30.!!"),
    ("signature with + and /", "Bearer e30.e30.+/+/"),
    ("two segments", "Bearer e30.e30"),
    ("four segments", "Bearer e30.e30.e30.e30"),
    ("empty header segment", "Bearer .e30."),
    ("empty claims segment", "Bearer e30.."),
    ("header not JSON", "Bearer " + seg("notjson") + ".e30."),
    ("header an array", "Bearer W10.e30."),
    ("claims an array", "Bearer e30.W10."),
    ("claims null", jwt("null")),
    ("claims a string", jwt('"x"')),
]

GRPC = [
    # (name, rpc, authorization, request, stream)
    ("GetDocument, bad name", "GetDocument", "Bearer garbage", {"name": f"{DB}/documents/c"}),
    ("GetDocument, reserved ID", "GetDocument", "Bearer garbage", {"name": f"{DB}/documents/__x__/a"}),
    ("GetDocument, empty name", "GetDocument", "Bearer garbage", {"name": ""}),
    ("GetDocument, bad mask", "GetDocument", "Bearer garbage", {"name": f"{DB}/documents/c/a", "mask": {"field_paths": ["a..b"]}}),
    ("GetDocument, no scheme", "GetDocument", "owner", {"name": f"{DB}/documents/c/a"}),
    ("GetDocument, no scheme, bad name", "GetDocument", "owner", {"name": f"{DB}/documents/c"}),
    ("GetDocument, OWNER", "GetDocument", "Bearer OWNER", {"name": f"{DB}/documents/c/a"}),
    ("GetDocument, user", "GetDocument", "Bearer e30.e30.", {"name": f"{DB}/documents/c/a"}),
    ("Commit, bad database", "Commit", "Bearer garbage", {"database": "projects/auth"}),
    ("Commit, bad write", "Commit", "Bearer garbage", {"database": DB, "writes": [{"update": {"name": f"{DB}/documents/c"}}]}),
    ("Commit, empty", "Commit", "Bearer garbage", {"database": DB}),
    ("BatchGetDocuments, bad database", "BatchGetDocuments", "Bearer garbage", {"database": "projects/auth"}),
    ("BatchGetDocuments, bad document", "BatchGetDocuments", "Bearer garbage", {"database": DB, "documents": [f"{DB}/documents/c"]}),
    ("BeginTransaction, bad database", "BeginTransaction", "Bearer garbage", {"database": "projects/auth"}),
    ("BeginTransaction", "BeginTransaction", "Bearer garbage", {"database": DB}),
    ("Rollback, bad database", "Rollback", "Bearer garbage", {"database": "projects/auth", "transaction": "AA=="}),
    ("Rollback, bad transaction", "Rollback", "Bearer garbage", {"database": DB, "transaction": "AA=="}),
    ("RunQuery, bad parent", "RunQuery", "Bearer garbage", {"parent": f"{DB}/documents/c", "structured_query": {"from": [{"collection_id": "c"}]}}),
    ("RunQuery, bad limit", "RunQuery", "Bearer garbage", {"parent": f"{DB}/documents", "structured_query": {"from": [{"collection_id": "c"}], "limit": -1}}),
    ("RunAggregationQuery, bad parent", "RunAggregationQuery", "Bearer garbage", {"parent": f"{DB}/documents/c", "structured_aggregation_query": {"structured_query": {"from": [{"collection_id": "c"}]}, "aggregations": [{"count": {}}]}}),
    ("RunAggregationQuery", "RunAggregationQuery", "Bearer garbage", {"parent": f"{DB}/documents", "structured_aggregation_query": {"structured_query": {"from": [{"collection_id": "c"}]}, "aggregations": [{"count": {}}]}}),
    ("ListDocuments, bad parent", "ListDocuments", "Bearer garbage", {"parent": f"{DB}/documents/c", "collection_id": "c"}),
    ("ListDocuments, reserved collection", "ListDocuments", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "__x__"}),
    ("ListDocuments showMissing, anonymous, reserved collection", "ListDocuments", None, {"parent": f"{DB}/documents", "collection_id": "__x__", "show_missing": True}),
    ("ListDocuments showMissing, user, order", "ListDocuments", "Bearer e30.e30.", {"parent": f"{DB}/documents", "collection_id": "c", "show_missing": True, "order_by": "a"}),
    ("ListDocuments showMissing, owner, order by name", "ListDocuments", "Bearer owner", {"parent": f"{DB}/documents", "collection_id": "c", "show_missing": True, "order_by": "__name__"}),
    ("ListDocuments showMissing, user", "ListDocuments", "Bearer e30.e30.", {"parent": f"{DB}/documents", "collection_id": "c", "show_missing": True}),
    ("ListDocuments, negative page size", "ListDocuments", "Bearer e30.e30.", {"parent": f"{DB}/documents", "collection_id": "c", "page_size": -1}),
    ("CreateDocument, bad parent", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents/c", "collection_id": "c", "document": {}}),
    ("CreateDocument, reserved collection", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "__x__", "document": {}}),
    ("CreateDocument, empty collection", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "", "document": {}}),
    ("CreateDocument, collection with a slash", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "a/b", "document": {}}),
    ("CreateDocument, collection ..", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "..", "document": {}}),
    ("CreateDocument, numeric collection", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "__id5__", "document": {}}),
    ("CreateDocument, document ID with a slash", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "c", "document_id": "a/b", "document": {}}),
    ("CreateDocument, document ID .", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "c", "document_id": ".", "document": {}}),
    ("CreateDocument, bad numeric document ID", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "c", "document_id": "__idx__", "document": {}}),
    ("GetDocument, reserved collection in a subcollection", "GetDocument", "Bearer garbage", {"name": f"{DB}/documents/c/a/__x__/b"}),
    ("GetDocument, numeric collection", "GetDocument", "Bearer garbage", {"name": f"{DB}/documents/__id5__/a"}),
    ("CreateDocument, reserved document ID", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "c", "document_id": "__x__", "document": {}}),
    ("CreateDocument", "CreateDocument", "Bearer garbage", {"parent": f"{DB}/documents", "collection_id": "c", "document": {}}),
    ("UpdateDocument, bad name", "UpdateDocument", "Bearer garbage", {"document": {"name": f"{DB}/documents/c"}}),
    ("UpdateDocument, no document", "UpdateDocument", "Bearer garbage", {}),
    ("UpdateDocument, no document, owner", "UpdateDocument", "Bearer owner", {}),
    ("UpdateDocument, bad mask", "UpdateDocument", "Bearer garbage", {"document": {"name": f"{DB}/documents/c/x"}, "update_mask": {"field_paths": ["a..b"]}}),
    ("DeleteDocument, bad name", "DeleteDocument", "Bearer garbage", {"name": f"{DB}/documents/c"}),
    ("DeleteDocument", "DeleteDocument", "Bearer garbage", {"name": f"{DB}/documents/c/x"}),
    ("BatchWrite, bad database", "BatchWrite", "Bearer garbage", {"database": "projects/auth"}),
    ("BatchWrite, anonymous, bad database", "BatchWrite", None, {"database": "projects/auth"}),
    ("BatchWrite, garbage", "BatchWrite", "Bearer garbage", {"database": DB}),
    ("BatchWrite, user, bad write", "BatchWrite", "Bearer e30.e30.", {"database": DB, "writes": [{"update": {"name": f"{DB}/documents/c"}}]}),
    ("BatchWrite, user, same document twice", "BatchWrite", "Bearer e30.e30.", {"database": DB, "writes": [{"delete": f"{DB}/documents/c/a"}, {"delete": f"{DB}/documents/c/a"}]}),
    ("BatchWrite, user", "BatchWrite", "Bearer e30.e30.", {"database": DB, "writes": [{"delete": f"{DB}/documents/c/a"}]}),
    ("ListCollectionIds, bad parent", "ListCollectionIds", "Bearer garbage", {"parent": f"{DB}/documents/c"}),
    ("ListCollectionIds, anonymous, bad parent", "ListCollectionIds", None, {"parent": f"{DB}/documents/c"}),
    ("ListCollectionIds, garbage", "ListCollectionIds", "Bearer garbage", {"parent": f"{DB}/documents"}),
    ("ListCollectionIds, user, negative page size", "ListCollectionIds", "Bearer e30.e30.", {"parent": f"{DB}/documents", "page_size": -1}),
    ("ListCollectionIds, user", "ListCollectionIds", "Bearer e30.e30.", {"parent": f"{DB}/documents"}),
    ("PartitionQuery, garbage", "PartitionQuery", "Bearer garbage", {"parent": f"{DB}/documents"}),
]

LISTEN = {"database": DB, "add_target": {"target_id": 1, "documents": {"documents": [f"{DB}/documents/c/a"]}}}
STREAMS = [
    # (name, rpc, authorization, request or None for a stream that sends nothing)
    ("Listen, garbage, nothing sent", "Listen", "Bearer garbage", None),
    ("Listen, garbage, bad database", "Listen", "Bearer garbage", {**LISTEN, "database": "projects/auth"}),
    ("Listen, no scheme", "Listen", "owner", None),
    ("Listen, owner", "Listen", "Bearer owner", LISTEN),
    ("Listen, OWNER", "Listen", "Bearer OWNER", LISTEN),
    ("Listen, mock user token", "Listen", jwt(MOCK), LISTEN),
    ("Listen, anonymous", "Listen", None, LISTEN),
    ("Write, garbage, nothing sent", "Write", "Bearer garbage", None),
    ("Write, garbage, bad database", "Write", "Bearer garbage", {"database": "projects/auth"}),
    ("Write, no scheme", "Write", "owner", None),
    ("Write, user", "Write", "Bearer e30.e30.", {"database": DB}),
]

E = f"/emulator/v1/{DB}/documents"
REST = [
    # (name, method, path, authorization, body)
    ("GET, garbage", "GET", DOCS + "/c/a", "Bearer garbage", None),
    ("GET, unknown parameter, garbage", "GET", DOCS + "/c/a?nope=1", "Bearer garbage", None),
    ("GET, no scheme", "GET", DOCS + "/c/a", "owner", None),
    ("GET, user", "GET", DOCS + "/c/a", jwt(MOCK), None),
    ("list, garbage", "GET", DOCS + "/c", "Bearer garbage", None),
    (":commit, bad payload, garbage", "POST", DOCS + ":commit", "Bearer garbage", "{nope"),
    (":commit, unknown field, no scheme", "POST", DOCS + ":commit", "owner", '{"nope":1}'),
    (":commit, no scheme", "POST", DOCS + ":commit", "owner", "{}"),
    (":commit, garbage", "POST", DOCS + ":commit", "Bearer garbage", "{}"),
    (":runQuery, bad parent, garbage", "POST", DOCS + "/c:runQuery", "Bearer garbage", "{}"),
    (":runAggregationQuery on a collection", "POST", DOCS + "/c:runAggregationQuery", None, "{}"),
    (":listCollectionIds on a collection", "POST", DOCS + "/c:listCollectionIds", None, "{}"),
    (":partitionQuery on a collection", "POST", DOCS + "/c:partitionQuery", None, "{}"),
    (":batchGet, bad document, garbage", "POST", DOCS + ":batchGet", "Bearer garbage", '{"documents":["x"]}'),
    (":listCollectionIds, user", "POST", DOCS + ":listCollectionIds", jwt(MOCK), "{}"),
    ("clear, garbage", "DELETE", E, "Bearer garbage", None),
    ("clear, no scheme", "DELETE", E, "owner", None),
    ("clear, user", "DELETE", E, "Bearer e30.e30.", None),
    ("clear with a slash, garbage", "DELETE", E + "/", "Bearer garbage", None),
    ("recursive delete, garbage", "DELETE", E + "/c/a", "Bearer garbage", None),
    ("recursive delete, reserved collection, garbage", "DELETE", E + "/__x__/a", "Bearer garbage", None),
    ("recursive delete, user", "DELETE", E + "/c/a", "Bearer e30.e30.", None),
    ("GET /, garbage", "GET", "/", "Bearer garbage", None),
    ("unknown verb, garbage", "POST", DOCS + ":nope", "Bearer garbage", "{}"),
    ("POST /reset, garbage", "POST", "/reset", "Bearer garbage", None),
]


def rest(method, path, authorization, body):
    c = http.client.HTTPConnection(*HOST.split(":"), timeout=10)
    headers = {"Content-Type": "application/json"}
    if authorization is not None:
        headers["Authorization"] = authorization
    c.request(method, path, body=body, headers=headers)
    r = c.getresponse()
    return {"status": r.status, "body": r.read().decode()}


def grpcurl(rpc, authorization, request, stream=False):
    args = ["grpcurl", "-plaintext", "-max-time", "3", "-import-path", "crates/hidane-proto/proto",
            "-proto", "google/firestore/v1/firestore.proto"]
    if stream:
        # Without it the official streams fail before reading the header (docs/parity-exceptions.md).
        args += ["-H", f"google-cloud-resource-prefix: {DB}"]
    if authorization is not None:
        args += ["-H", f"authorization: {authorization}"]
    args += ["-d", "" if request is None else json.dumps(request), HOST, f"google.firestore.v1.Firestore/{rpc}"]
    out = subprocess.run(args, capture_output=True, text=True, timeout=60)
    text = out.stdout + out.stderr
    m = re.search(r"Code: (\w+)\n\s*Message: (.*)", text)
    # A stream that answered before the deadline counts as accepted.
    if m and not (stream and out.stdout.strip() and m.group(1) == "DeadlineExceeded"):
        return {"code": re.sub(r"(?<!^)(?=[A-Z])", "_", m.group(1)).upper(), "message": m.group(2).strip()}
    return {"code": "OK"}


rest("POST", "/reset", None, None)
fixture = {
    "oracle": "cloud-firestore-emulator v1.22.0 (sha256 9b6498b7f62714d67f48f59b3818883cd682dbcd46b9f59511de81c97bb5166c)",
    "generated_by": "tools/oracle/auth.py",
    "classify": [
        {"name": n, "authorization": a, "outcome": rest("POST", DOCS + ":listCollectionIds", a, "{}")}
        for n, a in CLASSIFY
    ],
    "grpc": [
        {"name": n, "rpc": rpc, "authorization": a, "request": r, "outcome": grpcurl(rpc, a, r)}
        for n, rpc, a, r in GRPC
    ] + [
        {"name": n, "rpc": rpc, "authorization": a, "request": r, "stream": True, "outcome": grpcurl(rpc, a, r, True)}
        for n, rpc, a, r in STREAMS
    ],
    "rest": [
        {"name": n, "method": m, "path": p, "authorization": a, "body": b, "outcome": rest(m, p, a, b)}
        for n, m, p, a, b in REST
    ],
}
json.dump(fixture, sys.stdout, ensure_ascii=False, indent=1)
print()
