# admin-python

[google-cloud-firestore](https://pypi.org/project/google-cloud-firestore/) with pytest, against
hidane started on its own. [`test_firestore.py`](test_firestore.py) writes and reads a document,
queries, orders and counts, runs a transaction and listens to a query, each test on an empty
database.

## Run it

With Python 3.13 or newer and `hidane` on `PATH` (any channel in the
[README](../../README.md#install)):

```sh
python3 -m venv .venv
.venv/bin/pip install --require-hashes -r requirements.txt
hidane --host 127.0.0.1 --port 8080 &
FIRESTORE_EMULATOR_HOST=127.0.0.1:8080 .venv/bin/pytest
curl -X POST http://127.0.0.1:8080/shutdown   # stops hidane
```

With `FIRESTORE_EMULATOR_HOST` set, the client connects to the emulator at that address without
credentials; the tests fail with a message when it is not set. `demo-hidane` is a demo project ID,
which needs no Google Cloud project.

`requirements.txt` pins every package with its hashes. It is compiled from `requirements.in`:

```sh
uv pip compile --universal --python-version 3.13 --generate-hashes requirements.in -o requirements.txt
```

## In CI

[`examples.yml`](../../.github/workflows/examples.yml) installs `requirements.txt` on Python 3.13,
starts hidane the same way and runs `pytest`, with hidane built from the pull request and with the
published release that it pins (`PUBLISHED_VERSION`).
