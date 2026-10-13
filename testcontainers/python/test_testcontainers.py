"""hidane's container image through Testcontainers for Python (docs/testcontainers.md).

testcontainers-python has no Firestore module (its DatastoreContainer is for Datastore), so the
tests start the image as a generic DockerContainer, with the image's own command, wait for the
last line of hidane's banner, and point google-cloud-firestore at the mapped port through
FIRESTORE_EMULATOR_HOST. HIDANE_IMAGE names the image to test.
"""

import os
import threading
import urllib.request

import pytest
from google.cloud import firestore
from testcontainers.core.container import DockerContainer
from testcontainers.core.wait_strategies import LogMessageWaitStrategy

# A `demo-` project needs no Google Cloud project or credentials.
PROJECT = "demo-testcontainers"
# hidane's banner ends with this line, as the official emulator's does (a regular expression for
# LogMessageWaitStrategy: without the final period).
BANNER = "Dev App Server is now running"


@pytest.fixture(scope="module")
def emulator():
    """The container, started once for the module: its address as `host:port`."""
    image = os.environ.get("HIDANE_IMAGE")
    if not image:
        pytest.fail(
            "Set HIDANE_IMAGE to the hidane image to test, such as ghcr.io/hidane-dev/hidane:<version>"
        )
    container = (
        DockerContainer(image)
        .with_exposed_ports(8080)
        .waiting_for(LogMessageWaitStrategy(BANNER))
    )
    with container:
        yield f"{container.get_container_host_ip()}:{container.get_exposed_port(8080)}"


@pytest.fixture
def db(emulator, monkeypatch):
    """A client on an empty database. FIRESTORE_EMULATOR_HOST also tells the client not to look
    for credentials."""
    # The emulator's own endpoint deletes every document.
    url = f"http://{emulator}/emulator/v1/projects/{PROJECT}/databases/(default)/documents"
    with urllib.request.urlopen(urllib.request.Request(url, method="DELETE")) as response:
        assert response.status == 200
    monkeypatch.setenv("FIRESTORE_EMULATOR_HOST", emulator)
    client = firestore.Client(project=PROJECT)
    yield client
    client.close()


def add_users(db):
    users = db.collection("users")
    users.document("alovelace").set({"first": "Ada", "last": "Lovelace", "born": 1815})
    users.document("aturing").set({"first": "Alan", "last": "Turing", "born": 1912})
    users.document("ghopper").set({"first": "Grace", "last": "Hopper", "born": 1906})
    return users


def test_writes_and_reads_a_document(db):
    users = add_users(db)
    ada = users.document("alovelace").get()
    assert ada.to_dict() == {"first": "Ada", "last": "Lovelace", "born": 1815}
    assert not users.document("nobody").get().exists


def test_queries(db):
    users = add_users(db)
    modern = users.where(filter=firestore.FieldFilter("born", ">", 1900)).order_by("born")
    assert [doc.id for doc in modern.stream()] == ["ghopper", "aturing"]
    assert modern.count().get()[0][0].value == 2


def test_runs_a_transaction(db):
    counter = db.document("counters/visits")
    counter.set({"count": 0})

    @firestore.transactional
    def increment(transaction):
        snapshot = counter.get(transaction=transaction)
        transaction.update(counter, {"count": snapshot.get("count") + 1})

    for _ in range(3):
        increment(db.transaction())
    counter.update({"count": firestore.Increment(10)})
    assert counter.get().get("count") == 13


def test_commits_a_batch(db):
    users = add_users(db)
    batch = db.batch()
    batch.delete(users.document("aturing"))
    batch.update(users.document("ghopper"), {"rank": "Rear Admiral"})
    batch.commit()
    assert not users.document("aturing").get().exists
    assert users.document("ghopper").get().get("rank") == "Rear Admiral"


def test_listens_to_a_query(db):
    users = add_users(db)
    seen = []
    done = threading.Event()

    def on_snapshot(snapshot, changes, read_time):
        seen.append([doc.id for doc in snapshot])
        done.set()

    # Ascending: google-cloud-firestore sorts a listener's documents itself, and in ascending order
    # whatever the query's direction (2.34.1, base_query.py's _comparator).
    modern = users.where(filter=firestore.FieldFilter("born", ">", 1900)).order_by("born")
    watch = modern.on_snapshot(on_snapshot)
    try:
        assert done.wait(timeout=30), "the listener received no snapshot in 30 s"
    finally:
        watch.unsubscribe()
    assert seen[0] == ["ghopper", "aturing"]
