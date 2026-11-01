"""Tests of tools/production/indexes.py."""

import base64
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import indexes  # noqa: E402


def varint(n):
    out = b""
    while True:
        low, n = n & 0x7F, n >> 7
        out += bytes([low | (0x80 if n else 0)])
        if not n:
            return out


def field(number, value):
    if isinstance(value, int):
        return varint(number << 3) + varint(value)
    return varint(number << 3 | 2) + varint(len(value)) + value


def index_link(project, group, fields, scope=1):
    """The link production gives in an error to create an index: `fields` are (path, order),
    1 ascending, 2 descending, or (path, "contains")."""
    data = field(1, f"projects/{project}/databases/(default)/collectionGroups/{group}/indexes/_".encode())
    data += field(2, scope)
    for path, how in fields:
        data += field(3, field(1, path.encode()) + (field(3, 1) if how == "contains" else field(2, how)))
    encoded = base64.b64encode(data).decode()
    return f"https://console.firebase.google.com/v1/r/project/{project}/firestore/indexes?create_composite={encoded}"


def exemption_link(project, group, path, scope, order):
    """The link production gives to create an index of a single field."""
    data = field(1, f"projects/{project}/databases/(default)/collectionGroups/{group}/fields/{path}".encode())
    data += field(2, scope) + field(3, field(1, field(12, b"")) + field(2, order))
    encoded = base64.b64encode(data).decode()
    return f"https://console.firebase.google.com/v1/r/project/{project}/firestore/indexes?create_exemption={encoded}"


A = {"collectionGroup": "p", "queryScope": "COLLECTION",
     "fields": [{"fieldPath": "a", "order": "ASCENDING"}, {"fieldPath": "__name__", "order": "ASCENDING"}]}
GROUP_A = {"collectionGroup": "p", "fieldPath": "a", "indexes": [{"queryScope": "COLLECTION_GROUP", "order": "ASCENDING"}]}


class IndexesTest(unittest.TestCase):
    def test_an_index_is_read_from_the_answer(self):
        text = "The query requires an index. You can create it here: " + index_link(
            "own", "p", [("tags", "contains"), ("a", 2), ("__name__", 2)], scope=2)
        self.assertEqual(indexes.index_asked(text), {
            "collectionGroup": "p", "queryScope": "COLLECTION_GROUP",
            "fields": [{"fieldPath": "tags", "arrayConfig": "CONTAINS"}, {"fieldPath": "a", "order": "DESCENDING"},
                       {"fieldPath": "__name__", "order": "DESCENDING"}]})
        text = ("The query requires a COLLECTION_GROUP_ASC index for collection p and field a. You can create it here: "
                + exemption_link("own", "p", "a", 2, 1))
        self.assertEqual(indexes.index_asked(text), GROUP_A)
        text = ("Missing vector index configuration. Please create the required index with the following gcloud "
                "command: gcloud firestore indexes composite create --project=own --collection-group=t "
                """--query-scope=COLLECTION --field-config=vector-config='{"dimension":"2","flat": "{}"}',field-path=v""")
        vector = {"collectionGroup": "t", "queryScope": "COLLECTION", "fields": [
            {"fieldPath": "v", "vectorConfig": {"dimension": 2, "flat": {}}}]}
        self.assertEqual(indexes.index_asked(text), vector)
        # As production answers: the message inside an error inside an array, its quotes escaped.
        answer = json.dumps([{"error": {"code": 400, "message": text, "status": "FAILED_PRECONDITION"}}])
        self.assertEqual(indexes.index_asked(answer), vector)
        self.assertIsNone(indexes.index_asked(text.replace('"2"', '"two"')))
        # A field ordered before the vector, as for the documents' names in descending order.
        ordered = text.replace("--field-config=vector-config", "--field-config=order=DESCENDING,field-path=__name__ "
                                                               "--field-config=vector-config")
        self.assertEqual(indexes.index_asked(ordered), {**vector, "fields": [
            {"fieldPath": "__name__", "order": "DESCENDING"}, *vector["fields"]]})
        self.assertNotEqual(indexes.key(indexes.index_asked(ordered)), indexes.key(vector))
        self.assertIsNone(indexes.index_asked("no link"))
        self.assertIsNone(indexes.index_asked("…?create_composite=Q2tS"))

    def test_indexes_are_added_once(self):
        kept = indexes.INDEXES
        self.addCleanup(setattr, indexes, "INDEXES", kept)
        with tempfile.TemporaryDirectory() as tmp:
            indexes.INDEXES = Path(tmp) / "indexes.json"
            # As production lists it: named, without the collection group, without `__name__`.
            listed = {"name": "projects/x/databases/(default)/collectionGroups/p/indexes/CICAg", "queryScope": "COLLECTION",
                      "fields": [{"fieldPath": "a", "order": "ASCENDING"}], "state": "READY"}
            self.assertEqual(indexes.key(A), indexes.key(listed))
            vector = {"collectionGroup": "t", "queryScope": "COLLECTION",
                      "fields": [{"fieldPath": "v", "vectorConfig": {"dimension": 2, "flat": {}}}]}
            listed_vector = {"name": "projects/x/databases/(default)/collectionGroups/t/indexes/CICAg", "queryScope": "COLLECTION",
                             "fields": [{"fieldPath": "__name__", "order": "ASCENDING"}, *vector["fields"]]}
            self.assertEqual(indexes.key(vector), indexes.key(listed_vector))
            # A field before the vector: `__name__` comes between them, in its order.
            for order in ("ASCENDING", "DESCENDING"):
                filtered = {**vector, "fields": [{"fieldPath": "k", "order": order}, *vector["fields"]]}
                self.assertEqual(indexes.key(filtered), indexes.key({**listed_vector, "fields": [
                    {"fieldPath": "k", "order": order}, {"fieldPath": "__name__", "order": order}, *vector["fields"]]}))
            # A vector index of `__name__` itself is listed after the `__name__` production adds.
            of_name = {**vector, "fields": [{"fieldPath": "__name__", "vectorConfig": {"dimension": 2, "flat": {}}}]}
            self.assertEqual(indexes.key(of_name), indexes.key({**listed_vector, "fields": [
                {"fieldPath": "__name__", "order": "ASCENDING"}, *of_name["fields"]]}))
            by_name = {"collectionGroup": "p", "queryScope": "COLLECTION"}
            self.assertNotEqual(indexes.key(by_name | {"fields": [{"fieldPath": "__name__", "order": "DESCENDING"}]}),
                                indexes.key(by_name | {"fields": [{"fieldPath": "__name__", "order": "ASCENDING"}]}))
            report = Path(tmp) / "report.json"
            report.write_text(json.dumps({"recordings": [{"indexes": [A, GROUP_A]}, {"indexes": [A]}]}))
            self.assertEqual(indexes.add([report]), 2)
            self.assertEqual(indexes.add([report]), 0)
            self.assertEqual(json.loads(indexes.INDEXES.read_text()), {"indexes": [A], "fieldOverrides": [GROUP_A]})

    def test_a_datastore_mode_databases_indexes_are_kept_apart(self):
        kept = indexes.INDEXES, indexes.DATASTORE_INDEXES
        self.addCleanup(lambda: (setattr(indexes, "INDEXES", kept[0]), setattr(indexes, "DATASTORE_INDEXES", kept[1])))
        with tempfile.TemporaryDirectory() as tmp:
            indexes.INDEXES, indexes.DATASTORE_INDEXES = Path(tmp) / "indexes.json", Path(tmp) / "datastore_indexes.json"
            by_a = {"kind": "P", "ancestor": False, "properties": [{"name": "a", "direction": "asc"}, {"name": "b", "direction": "desc"}]}
            below = {"kind": "C", "ancestor": True, "properties": [{"name": "n", "direction": "asc"}]}
            by_key = {"kind": "T", "ancestor": False, "properties": [{"name": "__key__", "direction": "desc"}]}
            report = Path(tmp) / "report.json"
            report.write_text(json.dumps({"recordings": [{"indexes": [A], "datastoreIndexes": [by_a, below]},
                                                         {"datastoreIndexes": [by_a, by_key]}]}))
            self.assertEqual(indexes.add([report]), 4)
            self.assertEqual(indexes.add([report]), 0)
            self.assertEqual(json.loads(indexes.INDEXES.read_text())["indexes"], [A])
            self.assertEqual(sorted(map(indexes.datastore_key, indexes.load_datastore())),
                             sorted(map(indexes.datastore_key, [by_a, below, by_key])))
            # As the Firestore Admin API takes them, and lists them: with `__name__` at the end.
            self.assertEqual(indexes.admin(below), {"collectionGroup": "C", "queryScope": "COLLECTION_RECURSIVE",
                                                    "apiScope": "DATASTORE_MODE_API", "fields": [{"fieldPath": "n", "order": "ASCENDING"}]})
            listed = {"name": "projects/x/databases/datastore/collectionGroups/P/indexes/CICAg", "queryScope": "COLLECTION_GROUP",
                      "apiScope": "DATASTORE_MODE_API", "fields": [{"fieldPath": "a", "order": "ASCENDING"},
                                                                    {"fieldPath": "b", "order": "DESCENDING"},
                                                                    {"fieldPath": "__name__", "order": "ASCENDING"}]}
            self.assertEqual(indexes.key(indexes.admin(by_a)), indexes.key(listed))
            self.assertEqual(indexes.admin(by_key)["fields"], [{"fieldPath": "__name__", "order": "DESCENDING"}])
            project = indexes.Project("x", "a-token", "datastore")
            self.assertTrue(project.database.endswith("/databases/datastore"))
            self.assertEqual(len(project.wanted()), 3)

    def test_a_field_override_adds_to_what_the_field_has(self):
        defaults = [{"queryScope": "COLLECTION", "fields": [{"fieldPath": "a", "order": o}], "state": "READY"}
                    for o in ("ASCENDING", "DESCENDING")]
        self.assertEqual(indexes.Project.missing(GROUP_A, defaults), GROUP_A["indexes"])
        added = defaults + [{"queryScope": "COLLECTION_GROUP", "fields": [{"fieldPath": "a", "order": "ASCENDING"}]}]
        self.assertEqual(indexes.Project.missing(GROUP_A, added), [])


if __name__ == "__main__":
    unittest.main()
