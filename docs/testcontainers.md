# Testcontainers

hidane's container image, `ghcr.io/hidane-dev/hidane`, runs in Testcontainers in two ways:

- **In place of the Cloud SDK image** in the Firestore module of Testcontainers' gcloud modules.
  Tests that use the module (`FirestoreEmulatorContainer` in Java and Node, `firestore.Run` in Go)
  keep their code and change the image name. This needs hidane 0.5.0 or newer.
- **As a generic container**, with the image's own command and the last line of its banner. This
  works with older images too, and in Testcontainers for Python, which has no Firestore module.

Either way the image is the static hidane binary on an empty base. v0.6.0 is a 6.4 MB download
for linux/amd64, against 452 MB for `gcr.io/google.com/cloudsdktool/google-cloud-cli:emulators`,
which runs the official emulator on a JVM (compressed layers in each registry's manifest,
2026-10-16). hidane itself accepts connections within milliseconds of starting
([`docs/why.md`](why.md)).

## What is tested

[`testcontainers/`](../testcontainers/) holds a small test project per language. The
[Testcontainers workflow](../.github/workflows/testcontainers.yml) builds the image from each
commit, with [`packaging/docker/Dockerfile`](../packaging/docker/Dockerfile) as the release
workflow does, starts it through Testcontainers on GitHub's Ubuntu runners, and writes, reads,
queries, runs a transaction and a batch (a `BulkWriter` in Go, whose write batches are
deprecated), and listens to a query with the SDK. The tests also run against the published
v0.6.0 image.

| Language | Testcontainers module | With hidane's image | Tested with |
|---|---|---|---|
| Java | `org.testcontainers:testcontainers-gcloud`, `FirestoreEmulatorContainer` | in place of the Cloud SDK image (0.5.0 or newer) | Testcontainers 2.0.5, `google-cloud-firestore` 3.43.0, with the image from source and v0.6.0 ([`testcontainers/java`](../testcontainers/java/)) |
| Node | `@testcontainers/gcloud`, `FirestoreEmulatorContainer` | in place of the Cloud SDK image (0.5.0 or newer) | Testcontainers 12.2.0, `firebase-admin` 14.5.0, with the image from source and v0.6.0 ([`testcontainers/node`](../testcontainers/node/)) |
| Go | `github.com/testcontainers/testcontainers-go/modules/gcloud/firestore`, `firestore.Run` | in place of the Cloud SDK image (0.5.0 or newer) | Testcontainers 0.44.0, `cloud.google.com/go/firestore` 1.26.0, with the image from source and v0.6.0 ([`testcontainers/go`](../testcontainers/go/)) |
| Java, Node, Go | `GenericContainer` (`testcontainers.Run` in Go) | the image's own command (older images too) | the same projects, with the image from source and v0.6.0 |
| Python | `testcontainers`, `DockerContainer` (no Firestore module: `DatastoreContainer` is for Datastore) | the image's own command (older images too) | Testcontainers 4.15.0, `google-cloud-firestore` 2.34.1, with the image from source and v0.6.0 ([`testcontainers/python`](../testcontainers/python/)) |
| .NET | `Testcontainers.Firestore` | does not work yet: the module replaces the image's entrypoint with `gcloud`, which hidane's image does not have | |

## Java

In place of the Cloud SDK image. The module accepts the Cloud SDK images only, unless another is
declared compatible:

```java
import com.google.cloud.NoCredentials;
import com.google.cloud.firestore.Firestore;
import com.google.cloud.firestore.FirestoreOptions;
import org.testcontainers.gcloud.FirestoreEmulatorContainer;
import org.testcontainers.utility.DockerImageName;

// latest for brevity: pin 0.5.0 or newer.
DockerImageName hidane = DockerImageName.parse("ghcr.io/hidane-dev/hidane:latest")
        .asCompatibleSubstituteFor("gcr.io/google.com/cloudsdktool/google-cloud-cli");

try (FirestoreEmulatorContainer emulator = new FirestoreEmulatorContainer(hidane)) {
    emulator.start();
    Firestore db = FirestoreOptions.getDefaultInstance().toBuilder()
            .setProjectId("demo-test")
            .setHost(emulator.getEmulatorEndpoint())
            .setCredentials(NoCredentials.getInstance())
            .build()
            .getService();
    // …
}
```

`withFlags(…)` passes gcloud flags; the [flags hidane serves](#the-command-line-the-modules-pass)
are below. `org.testcontainers.containers.FirestoreEmulatorContainer`, the deprecated class kept
from Testcontainers 1.x, passes the same command, but is not tested here.

As a generic container, which works with images older than 0.5.0 too:

```java
import org.testcontainers.containers.GenericContainer;
import org.testcontainers.containers.wait.strategy.Wait;

try (GenericContainer<?> hidane = new GenericContainer<>("ghcr.io/hidane-dev/hidane:0.5")
        .withExposedPorts(8080)
        .waitingFor(Wait.forLogMessage(".*Dev App Server is now running.*", 1))) {
    hidane.start();
    String endpoint = hidane.getHost() + ":" + hidane.getMappedPort(8080);
    // FirestoreOptions as above, with setHost(endpoint).
}
```

## Node

In place of the Cloud SDK image:

```js
import { FirestoreEmulatorContainer } from "@testcontainers/gcloud";
import { initializeApp } from "firebase-admin/app";
import { getFirestore } from "firebase-admin/firestore";

// latest for brevity: pin 0.5.0 or newer.
const emulator = await new FirestoreEmulatorContainer("ghcr.io/hidane-dev/hidane:latest").start();
process.env.FIRESTORE_EMULATOR_HOST = emulator.getEmulatorEndpoint();
const db = getFirestore(initializeApp({ projectId: "demo-test" }));
// …
await emulator.stop();
```

`withFlag(name, value)` passes gcloud flags. Setting `FIRESTORE_EMULATOR_HOST` before the first
call also tells the SDK not to look for credentials; `db.settings({ host, ssl: false })`, as in
Testcontainers' own example, connects too, after a lookup of the metadata server that times out.

As a generic container, which works with images older than 0.5.0 too:

```js
import { GenericContainer, Wait } from "testcontainers";

const hidane = await new GenericContainer("ghcr.io/hidane-dev/hidane:0.5")
  .withExposedPorts(8080)
  .withWaitStrategy(Wait.forLogMessage("Dev App Server is now running."))
  .start();
process.env.FIRESTORE_EMULATOR_HOST = `${hidane.getHost()}:${hidane.getMappedPort(8080)}`;
```

## Go

In place of the Cloud SDK image, in a test:

```go
import (
	"cloud.google.com/go/firestore"
	"github.com/testcontainers/testcontainers-go"
	tcfirestore "github.com/testcontainers/testcontainers-go/modules/gcloud/firestore"
)

// latest for brevity: pin 0.5.0 or newer.
ctr, err := tcfirestore.Run(ctx, "ghcr.io/hidane-dev/hidane:latest", tcfirestore.WithProjectID("demo-test"))
testcontainers.CleanupContainer(t, ctr)
if err != nil {
	t.Fatal(err)
}
t.Setenv("FIRESTORE_EMULATOR_HOST", ctr.URI())
client, err := firestore.NewClient(ctx, ctr.ProjectID())
// …
```

Setting `FIRESTORE_EMULATOR_HOST` also tells the client not to look for credentials. The module's
own example, a gRPC connection without TLS passed with `option.WithGRPCConn`, connects too; both
are tested. The module also waits for the port, which testcontainers-go checks from inside the
container with a shell where there is one: hidane's image has none, so it checks from the host only
and logs `Shell not found in container`. `WithDatastoreMode()` passes
`--database-mode=datastore-mode`, which hidane serves (entities, transactions, queries,
aggregations, GQL and `--require_indexes`, [#81](https://github.com/hidane-dev/hidane/issues/81)); it is
not tested here. `gcloud.RunFirestore`, deprecated, passes
the same command as `firestore.Run`, but is not tested here.

As a generic container, which works with images older than 0.5.0 too:

```go
import "github.com/testcontainers/testcontainers-go/wait"

ctr, err := testcontainers.Run(ctx, "ghcr.io/hidane-dev/hidane:0.5",
	testcontainers.WithExposedPorts("8080/tcp"),
	testcontainers.WithWaitStrategy(wait.ForLog("Dev App Server is now running.")),
)
testcontainers.CleanupContainer(t, ctr)
if err != nil {
	t.Fatal(err)
}
endpoint, err := ctr.PortEndpoint(ctx, "8080/tcp", "")
// …
t.Setenv("FIRESTORE_EMULATOR_HOST", endpoint)
```

## Python

Testcontainers for Python has no Firestore module. hidane's image runs as a generic container:

```python
import os

from google.cloud import firestore
from testcontainers.core.container import DockerContainer
from testcontainers.core.wait_strategies import LogMessageWaitStrategy

with (
    DockerContainer("ghcr.io/hidane-dev/hidane:0.5")
    .with_exposed_ports(8080)
    .waiting_for(LogMessageWaitStrategy("Dev App Server is now running"))
) as hidane:
    os.environ["FIRESTORE_EMULATOR_HOST"] = (
        f"{hidane.get_container_host_ip()}:{hidane.get_exposed_port(8080)}"
    )
    db = firestore.Client(project="demo-test")
    # …
```

`LogMessageWaitStrategy` takes a regular expression. Setting `FIRESTORE_EMULATOR_HOST` before the
client is created also tells it not to look for credentials. In pytest, a module- or
session-scoped fixture that yields from the `with` block starts one container for many tests, as
[`testcontainers/python`](../testcontainers/python/test_testcontainers.py) does; the emulator's
`DELETE /emulator/v1/projects/{project}/databases/(default)/documents` empties the database between
tests.

## The command line the modules pass

The Firestore modules of Java, Node and Go expose port 8080, start the image with the command

```sh
/bin/sh -c "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 <flags>"
```

and wait for a log line that contains `running` (Go for the port too). hidane's image has no shell: its entrypoint is
hidane, which receives that command as its arguments. hidane reads the gcloud command line in the
script itself (words, single and double quotes and backslashes; no variables, operators,
redirections or globs), says on stderr which of its own flags it uses, and serves the emulator. Its
banner ends with `Dev App Server is now running.`, as the official emulator's does.

| `gcloud emulators firestore start` | hidane |
|---|---|
| `--host-port=[ADDRESS][:PORT]` | `--host ADDRESS --port PORT`; an IPv6 address goes in square brackets |
| `--database-mode=MODE` | `--database-mode MODE` (`datastore-mode` is served in part, [#81](https://github.com/hidane-dev/hidane/issues/81)) |
| `--edition=EDITION` | `--database-edition EDITION` |
| `--rules=FILE` | `--rules FILE` |
| `--import-data=PATH`, `--export-on-exit=DIR` | the same flags |
| `--index-file=FILE`, `--require-indexes` | `--index_file FILE`, `--require_indexes` |
| `--licenses` | `--licenses` |
| `--project`, `--verbosity`, `--quiet` / `-q` | accepted and not needed: hidane serves every project |

The `alpha` and `beta` tracks and the GA command are read alike, and a flag given twice takes its
last value, as in gcloud. Any other flag stops the container with exit code 2 and the flag's name;
a script that is not a `gcloud … emulators firestore start` command line stops it with exit code
127, as a shell does for a command it cannot find. Files such as a rules file must be in the
container, for example with `withCopyFileToContainer` (Java) or `withCopyFilesToContainer` (Node).
The Go module passes `--project=<id>` and, with `WithDatastoreMode()`, `--database-mode`; it has
no option for other flags.

## Running the tests

With Docker, against a published image or one built from a checkout (the workflow's `image` job
shows how):

```sh
cd testcontainers/java && HIDANE_IMAGE=ghcr.io/hidane-dev/hidane:0.6.0 mvn -B test
cd testcontainers/node && npm ci && HIDANE_IMAGE=ghcr.io/hidane-dev/hidane:0.6.0 npm test
cd testcontainers/go && HIDANE_IMAGE=ghcr.io/hidane-dev/hidane:0.6.0 go test ./...
cd testcontainers/python && python3 -m venv .venv && .venv/bin/pip install --require-hashes -r requirements.txt \
  && HIDANE_IMAGE=ghcr.io/hidane-dev/hidane:0.6.0 .venv/bin/pytest
```

The gcloud module tests need an image of 0.5.0 or newer. For an older image, run the generic
container tests only: `-Dtest=GenericContainerTest` (Java), `GENERIC_ONLY=1` (Node),
`-run TestGenericContainer` (Go).
