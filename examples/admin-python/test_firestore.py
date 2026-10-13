"""google-cloud-firestore against hidane, started on its own.

With FIRESTORE_EMULATOR_HOST set, the client talks to the emulator at that address, with no
credentials. Start hidane first (`hidane --host 127.0.0.1 --port 8080`), then run
`FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 pytest`.
"""

import os
import threading
import urllib.request

import pytest
from google.cloud import firestore

# A `demo-` project needs no Google Cloud project or credentials.
PROJECT = "demo-hidane"


@pytest.fixture
def db():
    host = os.environ.get("FIRESTORE_EMULATOR_HOST")
    if not host:
        pytest.fail("Set FIRESTORE_EMULATOR_HOST to hidane's address, such as 127.0.0.1:8080")
    # Each test starts from an empty database: the emulator's own endpoint deletes every document.
    url = f"http://{host}/emulator/v1/projects/{PROJECT}/databases/(default)/documents"
    with urllib.request.urlopen(urllib.request.Request(url, method="DELETE")) as response:
        assert response.status == 200
    client = firestore.Client(project=PROJECT)
    yield client
    client.close()


def test_writes_and_reads_a_document(db):
    ref = db.document("cities/tokyo")
    ref.set({"name": "Tokyo", "population": 14_000_000, "updated": firestore.SERVER_TIMESTAMP})

    snapshot = ref.get()
    assert snapshot.get("name") == "Tokyo"
    assert snapshot.get("updated").timestamp() > 0


def test_queries_orders_and_counts(db):
    cities = db.collection("cities")
    cities.document("tokyo").set({"name": "Tokyo", "population": 14_000_000})
    cities.document("osaka").set({"name": "Osaka", "population": 2_700_000})
    cities.document("nara").set({"name": "Nara", "population": 350_000})

    large = cities.where(filter=firestore.FieldFilter("population", ">", 1_000_000))
    ordered = large.order_by("population", direction=firestore.Query.DESCENDING)
    assert [doc.id for doc in ordered.stream()] == ["tokyo", "osaka"]
    assert large.count().get()[0][0].value == 2


def test_increments_a_counter_in_a_transaction(db):
    ref = db.document("counters/visits")
    ref.set({"n": 41})

    @firestore.transactional
    def increment(transaction):
        snapshot = ref.get(transaction=transaction)
        transaction.update(ref, {"n": snapshot.get("n") + 1})

    increment(db.transaction())
    assert ref.get().get("n") == 42


def test_a_listener_receives_the_documents_its_query_matches(db):
    messages = db.collection("messages")
    received = []
    done = threading.Event()

    def on_snapshot(snapshot, changes, read_time):
        if len(snapshot) == 2:
            received.extend(sorted(doc.get("text") for doc in snapshot))
            done.set()

    watch = messages.where(filter=firestore.FieldFilter("room", "==", "general")).on_snapshot(
        on_snapshot
    )
    messages.add({"room": "general", "text": "hello"})
    messages.add({"room": "random", "text": "elsewhere"})
    messages.add({"room": "general", "text": "world"})
    try:
        assert done.wait(timeout=10), "the listener did not receive both documents in 10 s"
    finally:
        watch.unsubscribe()
    assert received == ["hello", "world"]
