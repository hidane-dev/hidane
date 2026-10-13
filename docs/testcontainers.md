# Testcontainers

hidane's container image, `ghcr.io/hidane-dev/hidane`, runs in Testcontainers in two ways:

- **In place of the Cloud SDK image** in the Firestore module of Testcontainers' gcloud modules.
  Tests that use `FirestoreEmulatorContainer` keep their code and change the image name. This
  needs an image newer than v0.4.0: the release after v0.4.0 is the first to have it.
- **As a generic container**, with the image's own command and the last line of its banner. This
  works with the published images, v0.4.0 included.

Either way the image is the static hidane binary on an empty base. v0.4.0 is a 4.9 MB download
for linux/amd64, against 446 MB for `gcr.io/google.com/cloudsdktool/google-cloud-cli:emulators`,
which runs the official emulator on a JVM (compressed layers in each registry's manifest,
2026-10-13). hidane itself accepts connections within milliseconds of starting
([`docs/why.md`](why.md)).

## What is tested

[`testcontainers/`](../testcontainers/) holds a small test project per language. The
[Testcontainers workflow](../.github/workflows/testcontainers.yml) builds the image from each
commit, with [`packaging/docker/Dockerfile`](../packaging/docker/Dockerfile) as the release
workflow does, starts it through Testcontainers on GitHub's Ubuntu runners, and writes, reads,
queries, runs a transaction and a batch, and listens to a query with the SDK. The generic
container tests also run against the published v0.4.0 image.

| Language | Testcontainers module | With hidane's image | Tested with |
|---|---|---|---|
| Java | `org.testcontainers:testcontainers-gcloud`, `FirestoreEmulatorContainer` | in place of the Cloud SDK image (newer than v0.4.0) | Testcontainers 2.0.5, `google-cloud-firestore` 3.43.0 ([`testcontainers/java`](../testcontainers/java/)) |
| Node | `@testcontainers/gcloud`, `FirestoreEmulatorContainer` | in place of the Cloud SDK image (newer than v0.4.0) | Testcontainers 12.2.0, `firebase-admin` 14.5.0 ([`testcontainers/node`](../testcontainers/node/)) |
| Java, Node | `GenericContainer` | the image's own command (every published image) | the same projects, with the image from source and v0.4.0 |
| Go | `modules/gcloud/firestore` | not tested yet: it starts the image with the same command line as Java and Node | |
| .NET | `Testcontainers.Firestore` | does not work yet: the module replaces the image's entrypoint with `gcloud`, which hidane's image does not have | |
| Python | no Firestore module (`DatastoreContainer` is for Datastore) | as a `DockerContainer`, the generic way: not tested yet | |

## Java

In place of the Cloud SDK image. The module accepts the Cloud SDK images only, unless another is
declared compatible:

```java
import com.google.cloud.NoCredentials;
import com.google.cloud.firestore.Firestore;
import com.google.cloud.firestore.FirestoreOptions;
import org.testcontainers.gcloud.FirestoreEmulatorContainer;
import org.testcontainers.utility.DockerImageName;

// latest for brevity: pin a version newer than 0.4.0.
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

As a generic container, with any published image:

```java
import org.testcontainers.containers.GenericContainer;
import org.testcontainers.containers.wait.strategy.Wait;

try (GenericContainer<?> hidane = new GenericContainer<>("ghcr.io/hidane-dev/hidane:0.4")
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

// latest for brevity: pin a version newer than 0.4.0.
const emulator = await new FirestoreEmulatorContainer("ghcr.io/hidane-dev/hidane:latest").start();
process.env.FIRESTORE_EMULATOR_HOST = emulator.getEmulatorEndpoint();
const db = getFirestore(initializeApp({ projectId: "demo-test" }));
// …
await emulator.stop();
```

`withFlag(name, value)` passes gcloud flags. Setting `FIRESTORE_EMULATOR_HOST` before the first
call also tells the SDK not to look for credentials; `db.settings({ host, ssl: false })`, as in
Testcontainers' own example, connects too, after a lookup of the metadata server that times out.

As a generic container, with any published image:

```js
import { GenericContainer, Wait } from "testcontainers";

const hidane = await new GenericContainer("ghcr.io/hidane-dev/hidane:0.4")
  .withExposedPorts(8080)
  .withWaitStrategy(Wait.forLogMessage("Dev App Server is now running."))
  .start();
process.env.FIRESTORE_EMULATOR_HOST = `${hidane.getHost()}:${hidane.getMappedPort(8080)}`;
```

## The command line the modules pass

The Firestore modules of Java, Node and Go expose port 8080, start the image with the command

```sh
/bin/sh -c "gcloud beta emulators firestore start --host-port 0.0.0.0:8080 <flags>"
```

and wait for a log line that contains `running`. hidane's image has no shell: its entrypoint is
hidane, which receives that command as its arguments. hidane reads the gcloud command line in the
script itself (words, single and double quotes and backslashes; no variables, operators,
redirections or globs), says on stderr which of its own flags it uses, and serves the emulator. Its
banner ends with `Dev App Server is now running.`, as the official emulator's does.

| `gcloud emulators firestore start` | hidane |
|---|---|
| `--host-port=[ADDRESS][:PORT]` | `--host ADDRESS --port PORT`; an IPv6 address goes in square brackets |
| `--database-mode=MODE` | `--database-mode MODE` (`datastore-mode` is not supported yet, [#81](https://github.com/hidane-dev/hidane/issues/81)) |
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

## Running the tests

With Docker, against a published image or one built from a checkout (the workflow's `image` job
shows how):

```sh
cd testcontainers/java && HIDANE_IMAGE=ghcr.io/hidane-dev/hidane:0.4.0 mvn -B test -Dtest=GenericContainerTest
cd testcontainers/node && npm ci && HIDANE_IMAGE=ghcr.io/hidane-dev/hidane:0.4.0 GENERIC_ONLY=1 npm test
```

Without `-Dtest` / `GENERIC_ONLY`, the gcloud module tests run too, which need an image newer than
v0.4.0.
