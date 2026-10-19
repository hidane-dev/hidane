#!/usr/bin/env python3
"""The indexes production needs to run the recordings' queries (#82).

Production refuses a query that needs an index the database does not have, and says how to make
it; the emulators need none (D070). The replay reads the index from production's answer and lists
it in its report (`indexes`). This keeps those indexes in `indexes.json` and creates them in the
project.

Usage (from the repository root):
    python3 -I -B tools/production/indexes.py add production-parity/report.json
    PRODUCTION_ACCESS_TOKEN=… python3 -I -B tools/production/indexes.py create --project <id> --wait

`indexes.json` is in the format of `firestore.indexes.json`, but a field override lists only the
indexes to add to the field's own: `create` adds them to what the field has. `create` makes what
the project does not have yet. It needs a token that may change indexes, the project owner's: the
workflow's service account may only read and write documents. Stdlib only.
"""

import argparse
import base64
import binascii
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

INDEXES = Path(__file__).resolve().parent / "indexes.json"
TOKEN_ENV = "PRODUCTION_ACCESS_TOKEN"
ADMIN = "https://firestore.googleapis.com/v1"
LINK = re.compile(r"create_(composite|exemption)=([A-Za-z0-9_\-+/=%]+)")
VECTOR = re.compile(r"--collection-group=(\S+) --query-scope=(\S+) ((?:--field-config=\S+ )*)"
                    r"--field-config=vector-config='(\{[^']*\})',field-path=(\S+)")
# A field of a vector index's command before the vector: `order=DESCENDING,field-path=__name__`.
FIELD_CONFIG = re.compile(r"--field-config=(order|array-config)=(\w+),field-path=(\S+)")
SCOPES = {1: "COLLECTION", 2: "COLLECTION_GROUP", 3: "COLLECTION_RECURSIVE"}
ORDERS = {1: "ASCENDING", 2: "DESCENDING"}


def message(data):
    """The fields of a protobuf message, `{number: [value, …]}`: an int for a varint, bytes for a
    length-delimited field."""
    fields, at = {}, 0

    def varint():
        nonlocal at
        value = shift = 0
        while True:
            byte = data[at]
            at += 1
            value |= (byte & 0x7F) << shift
            shift += 7
            if byte < 0x80:
                return value

    while at < len(data):
        key = varint()
        number, wire = key >> 3, key & 7
        if wire == 0:
            value = varint()
        elif wire == 2:
            length = varint()
            value, at = data[at:at + length], at + length
        elif wire in (1, 5):
            size = 8 if wire == 1 else 4
            value, at = data[at:at + size], at + size
        else:
            raise ValueError(f"protobuf wire type {wire}")
        fields.setdefault(number, []).append(value)
    return fields


def how(spec):
    """An index field's order (field 2) or array config (field 3)."""
    if 2 in spec:
        return {"order": ORDERS[spec[2][0]]}
    if 3 in spec:
        return {"arrayConfig": "CONTAINS"}
    raise ValueError("an index field without an order or an array config")


def composite(index):
    """The `Index` (google.firestore.admin.v1) of a `create_composite` link."""
    name = index[1][0].decode()
    fields = []
    for raw in index.get(3, []):
        field = message(raw)
        entry = {"fieldPath": field[1][0].decode()}
        if 4 in field:
            entry["vectorConfig"] = {"dimension": message(field[4][0]).get(1, [0])[0], "flat": {}}
        else:
            entry.update(how(field))
        fields.append(entry)
    return {"collectionGroup": name.split("/collectionGroups/")[1].split("/")[0],
            "queryScope": SCOPES[index.get(2, [1])[0]], "fields": fields}


def exemption(field):
    """A `create_exemption` link's: one index of a single field, named by the field (as
    `…/collectionGroups/p/fields/a`), with its query scope and its order or array config."""
    group, path = field[1][0].decode().split("/collectionGroups/")[1].split("/fields/")
    return {"collectionGroup": group, "fieldPath": path,
            "indexes": [{"queryScope": SCOPES[field[2][0]], **how(message(field[3][0]))}]}


def messages(text):
    """The error messages of an answer: of its error, or of each error of a stream; or the text,
    if it is not JSON."""
    try:
        answer = json.loads(text)
    except ValueError:
        return [text]
    found = [r["error"].get("message", "") for r in (answer if isinstance(answer, list) else [answer])
             if isinstance(r, dict) and isinstance(r.get("error"), dict)]
    return found or [text]


def index_asked(text):
    """The index production asks for in an answer, or None: from its link to create the index
    (a composite index, or one of a single field), or from the gcloud command it gives for a
    vector index. One that does not read is None: a replay goes on without it."""
    for said in messages(text):
        try:
            vector = VECTOR.search(said)
            if vector:
                group, scope, before, config, path = vector.groups()
                dimension = int(json.loads(config)["dimension"])
                fields = [{"fieldPath": field, "order" if kind == "order" else "arrayConfig": how}
                          for kind, how, field in FIELD_CONFIG.findall(before)]
                return {"collectionGroup": group, "queryScope": scope,
                        "fields": fields + [{"fieldPath": path, "vectorConfig": {"dimension": dimension, "flat": {}}}]}
            found = LINK.search(said)
            if found:
                encoded = urllib.parse.unquote(found.group(2)).replace("-", "+").replace("_", "/")
                decoded = message(base64.b64decode(encoded + "=" * (-len(encoded) % 4)))
                return composite(decoded) if found.group(1) == "composite" else exemption(decoded)
        except (binascii.Error, ValueError, KeyError, IndexError, TypeError, UnicodeDecodeError):
            return None
    return None


def key(index):
    """What makes two indexes the same. Production adds `__name__` to a composite index it lists
    or asks for: at the end, or in a vector index just before the vector, in the order of the
    field before it (ascending if none); one without it is the same index. A vector index of the
    field `__name__` itself keeps its own."""
    group = index.get("collectionGroup") or index["name"].split("/collectionGroups/")[1].split("/")[0]
    if "fieldPath" in index:
        return json.dumps([group, index["fieldPath"], index["indexes"]], sort_keys=True)
    fields = [{k: v for k, v in f.items() if k in ("fieldPath", "order", "arrayConfig", "vectorConfig")}
              for f in index["fields"]]
    if len(fields) > 1 and fields[-1]["fieldPath"] == "__name__" and "vectorConfig" not in fields[-1]:
        fields = fields[:-1]
    at = next((n for n, f in enumerate(fields) if "vectorConfig" in f), 0)
    if at and fields[at - 1] == {"fieldPath": "__name__", "order": fields[at - 2]["order"] if at > 1 else "ASCENDING"}:
        fields = fields[:at - 1] + fields[at:]
    return json.dumps([group, index["queryScope"], fields], sort_keys=True)


def load():
    """Every index of `indexes.json`: the composite ones, then the field overrides."""
    if not INDEXES.exists():
        return []
    data = json.loads(INDEXES.read_text())
    return data["indexes"] + data["fieldOverrides"]


def save(indexes):
    unique = {key(index): index for index in indexes}
    ordered = [unique[k] for k in sorted(unique)]
    INDEXES.write_text(json.dumps({"indexes": [i for i in ordered if "fieldPath" not in i],
                                   "fieldOverrides": [i for i in ordered if "fieldPath" in i]}, indent=1) + "\n")
    return ordered


def add(reports):
    """Adds the indexes the reports' queries asked for; answers how many were new."""
    indexes = load()
    before = {key(index) for index in indexes}
    for report in reports:
        for recording in json.loads(Path(report).read_text())["recordings"]:
            indexes += recording.get("indexes", [])
    return len({key(index) for index in save(indexes)} - before)


def request(method, url, token, body=None):
    req = urllib.request.Request(url, method=method, data=None if body is None else json.dumps(body).encode(),
                                 headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=60) as res:
            return json.loads(res.read() or b"{}")
    except urllib.error.HTTPError as err:
        raise RuntimeError(f"{method} {url.split('/databases/')[-1]} answered {err.code}: "
                           f"{err.read().decode(errors='replace')[:300]}") from None


class Project:
    """The indexes of a project's `(default)` database, through the Firestore Admin API."""

    def __init__(self, project, token):
        self.database = f"{ADMIN}/projects/{project}/databases/(default)"
        self.token = token

    def composites(self):
        """The composite indexes: their states by key."""
        found, page = {}, None
        while True:
            url = f"{self.database}/collectionGroups/-/indexes"
            answer = request("GET", url + (f"?pageToken={urllib.parse.quote(page)}" if page else ""), self.token)
            for index in answer.get("indexes", []):
                found[key(index)] = index.get("state")
            page = answer.get("nextPageToken")
            if not page:
                return found

    def field(self, override):
        """A field's indexes, its own or those it takes from the database's defaults."""
        url = f"{self.database}/collectionGroups/{override['collectionGroup']}/fields/{override['fieldPath']}"
        return url, request("GET", url, self.token).get("indexConfig", {}).get("indexes", [])

    @staticmethod
    def missing(override, indexes):
        """The indexes of a field override that are not among a field's `indexes`."""
        have = [{k: v for k, v in i["fields"][0].items() if k != "fieldPath"} | {"queryScope": i["queryScope"]}
                for i in indexes]
        return [i for i in override["indexes"] if i not in have]

    def create(self):
        """Creates the indexes of `indexes.json` the database does not have; answers how many."""
        made = 0
        have = self.composites()
        for index in load():
            if "fieldPath" in index:
                url, indexes = self.field(index)
                missing = self.missing(index, indexes)
                if missing:
                    kept = [{"queryScope": i["queryScope"], "fields": i["fields"]} for i in indexes]
                    added = [{"queryScope": i["queryScope"], "fields": [
                        {"fieldPath": index["fieldPath"], **{k: v for k, v in i.items() if k != "queryScope"}}]}
                        for i in missing]
                    request("PATCH", f"{url}?updateMask=indexConfig", self.token,
                            {"indexConfig": {"indexes": kept + added}})
                    made += len(missing)
            elif key(index) not in have:
                request("POST", f"{self.database}/collectionGroups/{index['collectionGroup']}/indexes", self.token,
                        {"queryScope": index["queryScope"], "fields": index["fields"]})
                made += 1
        return made

    def building(self):
        """How many indexes of `indexes.json` are not ready yet."""
        states = self.composites()
        count = 0
        for index in load():
            if "fieldPath" in index:
                _, indexes = self.field(index)
                count += len(self.missing(index, [i for i in indexes if i.get("state") == "READY"]))
            elif states.get(key(index)) != "READY":
                count += 1
        return count


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("add", help="add the indexes reports asked for").add_argument("reports", nargs="+")
    make = commands.add_parser("create", help="create the indexes the project does not have")
    make.add_argument("--project", required=True)
    make.add_argument("--wait", action="store_true", help="wait until every index is ready")
    args = parser.parse_args(argv)
    if args.command == "add":
        print(f"{add(args.reports)} new indexes; {len(load())} in {INDEXES.name}", file=sys.stderr)
        return
    token = os.environ.get(TOKEN_ENV)
    if not token:
        parser.error(f"{TOKEN_ENV} is not set")
    project = Project(args.project, token)
    print(f"{project.create()} indexes created", file=sys.stderr)
    while args.wait:
        building = project.building()
        if not building:
            break
        print(f"{building} indexes building", file=sys.stderr)
        time.sleep(30)


if __name__ == "__main__":
    main()
