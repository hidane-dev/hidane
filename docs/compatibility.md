# Client compatibility

Which clients can talk to hidane depends on the transport each SDK uses when it is pointed at an
emulator. Phase 0 read the SDK sources to settle this; the conclusions are below. Status reflects
the [ROADMAP](ROADMAP.md): nothing is implemented yet.

Status legend: **planned (v0.1)** gRPC only · **planned (v0.2)** Security Rules · **planned (v0.3)** WebChannel

## Transport required by each client

| Client | Transport used against an emulator | hidane status |
|---|---|---|
| firebase-admin (Node) / `@google-cloud/firestore` (default settings) | gRPC (plaintext). `listen` is always gRPC even with `preferRest` | planned (v0.1) |
| `@google-cloud/firestore` with `preferRest: true` or `FIRESTORE_PREFER_REST=true` | REST for unary calls, gRPC for `listen` | planned (v0.1, REST is a v0.1 stretch item) |
| google-cloud-go `firestore` | gRPC. `NewRESTClient` does not support the emulator | planned (v0.1) |
| google-cloud-firestore (Python) | gRPC | planned (v0.1) |
| java-firestore | gRPC | planned (v0.1) |
| firebase-ios-sdk (iOS, macOS, tvOS, watchOS) | gRPC. Uses the Listen and Write streams | planned (v0.1) |
| firebase-android-sdk | gRPC (grpc-okhttp). Uses the Listen and Write streams | planned (v0.1) |
| FlutterFire `cloud_firestore` on Android / iOS / macOS / Windows | Native SDK (gRPC) | planned (v0.1) |
| Firebase C++ SDK / Unity | iOS C++ core or Android SDK (gRPC) | planned (v0.1) |
| firebase-js-sdk imported **in Node** | gRPC (`@grpc/grpc-js`) | planned (v0.1) |
| firebase-js-sdk **Lite** (`firebase/firestore/lite`, any platform) | REST (`fetch`) only | planned (v0.1 stretch: REST) |
| firebase-js-sdk **in the browser** | **WebChannel** for Listen and Write (this includes `getDoc`, `getDocs`, `setDoc`, `updateDoc`, `deleteDoc`, `writeBatch`), REST for `runTransaction`, aggregate queries and pipelines | planned (v0.3) |
| FlutterFire `cloud_firestore_web` | Same as firebase-js-sdk in the browser | planned (v0.3) |
| Emulator UI, `curl`, `firebase-tools` itself | REST and emulator-specific HTTP endpoints | planned (v0.1) |

So with v0.1 (gRPC only) the server-side SDKs, the mobile SDKs and Node-based test suites work.
**Browser apps do not work until v0.3**, because the Web SDK does almost everything over
WebChannel and the rest over REST.

## How each client finds the emulator

| Client | Reads `FIRESTORE_EMULATOR_HOST` | Explicit API | TLS |
|---|---|---|---|
| firebase-js-sdk | **No.** Reads `__FIREBASE_DEFAULTS__` (`emulatorHosts.firestore`) at `getFirestore()` | `connectFirestoreEmulator(db, host, port, { mockUserToken })` | disabled automatically |
| firebase-admin (Node) / `@google-cloud/firestore` | Yes (takes precedence over settings) | `settings({ host, ssl: false })` | disabled via the env var path |
| Go | Yes | none (env var only) | insecure credentials automatically |
| Python | Yes | none (env var only) | insecure channel automatically |
| Java | Yes (`setEmulatorHost` takes precedence) | `FirestoreOptions.Builder.setEmulatorHost(host)` | plaintext automatically |
| iOS | No | `useEmulatorWithHost:port:` **only changes the host**; set `settings.isSSLEnabled = false` yourself | manual |
| Android | No | `useEmulator(host, port)` also sets `sslEnabled(false)` | automatic |
| Flutter | No | `useFirestoreEmulator(host, port, sslEnabled: false)` | native: defaults to false; web: delegated to js-sdk |

## Authentication header each client sends to an emulator

| Client | `Authorization` header |
|---|---|
| firebase-admin (Node), Go, Python, Java | `Bearer owner` (treated as administrator) |
| firebase-js-sdk | With `mockUserToken`: an unsigned `alg: none` JWT, or a string token as given (rules-unit-testing's `withSecurityRulesDisabled` passes `owner`). Otherwise the Firebase Auth token, or no header |
| iOS, Android | Only the signed-in user's token, otherwise no header |

The official emulator treats `Bearer owner` and Google OAuth access tokens (`Bearer ya29.…`) as
an administrator, ignoring case. Any other bearer token must be a JWT, which it reads without
verification (any `alg` and signature, no expiry, audience or subject check) for
`request.auth`; a token that is not three URL-safe base64 segments with JSON objects in the
first two fails with `INVALID_ARGUMENT` "invalid jwt", and a header that is not a bearer token
fails with `UNKNOWN` (HTTP 500). Without a header, Security Rules see `request.auth == null`.
Each RPC reads the header after parsing the resource it names and before validating the rest
of the request; Listen and Write read it when the stream opens. hidane follows the same
contract (`tools/oracle/auth.py`).

## RPCs each client actually uses

| Client | Listen stream | Write stream | Commit | BatchGet | RunQuery | RunAggregationQuery | ListCollectionIds | ListDocuments | BeginTransaction | Rollback | BatchWrite | PartitionQuery | ExecutePipeline |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| js-sdk (browser) | WebChannel | WebChannel | REST (transactions) | REST (transactions) | — | REST | — | — | — | — | — | — | REST |
| js-sdk Lite | — | — | REST | REST | REST | REST | — | — | — | — | — | — | REST |
| js-sdk (Node) | gRPC | gRPC | gRPC (transactions) | gRPC (transactions) | — | gRPC | — | — | — | — | — | — | gRPC |
| admin (Node) | gRPC | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | — (`new_transaction` on first read) | ✓ | ✓ | — | ✓ |
| Go / Python / Java | gRPC | — | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| iOS / Android | gRPC | gRPC | ✓ (transactions) | ✓ (transactions) | — | ✓ | — | — | — | — | — | — | ✓ |

Notable consequences for the implementation:

- The JS SDKs never call `BeginTransaction`; transactions are `BatchGetDocuments` plus a `Commit` with preconditions. A document read but not written goes into the commit as a `verify` write (`Write.verify`, field 5), which googleapis' published protos lack; the mobile SDKs do the same.
- The Admin SDK starts transactions lazily with the `new_transaction` option on the first read.
- Mobile SDKs run queries through the Listen stream, not `RunQuery`.
- `PartitionQuery` is only used by the Go / Python / Java clients; the official emulator answers it with 501.

Sources: firebase-js-sdk `packages/firestore/src/platform/*/connection.ts`, `remote/datastore.ts`,
`remote/persistent_stream.ts`; google-cloud-node `handwritten/firestore/dev/src/index.ts`;
google-cloud-go `firestore/client.go`; google-cloud-python `firestore_v1/base_client.py`;
java-firestore `FirestoreOptions.java`; firebase-ios-sdk `Firestore/core/src/remote/*.cc`;
firebase-android-sdk `firebase-firestore/.../remote/*.java`; flutterfire `packages/cloud_firestore/*`.
