# Oracle provenance

Every behaviour hidane takes from the official emulator rests on a recording: a file in
`crates/*/tests/fixtures/` that a test replays, or an official transcript in `results/` that is
compared with hidane's. This page says what each recording records about where it came from:
which script made it, against which release of the official emulator, and when.

Every recording was made by running the emulator. Some facts hidane relies on are not in a
recording: the emulator's flag and environment variable names, the names of its endpoints and
messages, such as the fields of its import and export requests. Some of these may have come from
the static inspections of the jar made before this policy
([OBSERVATION_POLICY.md](OBSERVATION_POLICY.md#history-before-this-policy)); most are also
observable at runtime or in firebase-tools' public source, but not each has been re-verified
([SOURCES.md](SOURCES.md#7-before-this-policy)).

## What a recording says about itself today

| Recordings | Script | Official emulator | When |
|---|---|---|---|
| Fixtures that are a JSON object with a header (most of them, such as `queries.json`, `rest.json`, `transactions.json`) | `generated_by`: the script, such as `"tools/oracle/queries.py"` | `oracle`: `"cloud-firestore-emulator v1.22.0 (sha256 9b6498b7…)"`, the release and its jar's SHA-256 | git history |
| Fixtures without one: the lists of steps the Security Rules scripts record through `tools/oracle/steps.py` (`rules_*.json`, `websocket_requests.json`), and `export_import.json`, `field_order.json`, `rules_eval.json`, `rules_paths.json`, `rules_warnings.json`, whose scripts came before the header | The table in [`tools/oracle/README.md`](../../tools/oracle/README.md) | Not in the file; git history shows the release the repository used when each was recorded (below) | git history |
| `results/rules-conditions-official-v1.22.0.json`, replayed by a test | The table in `tools/oracle/README.md` (`rules_conditions.py`) | The file name: `-official-v1.22.0` | git history |
| Official SDK transcripts, `results/<name>-official-v<version>.json` | The prose of `tools/oracle/README.md` names the script that prints each | The file name | git history |

The `oracle` header is a constant each script writes; it names the release the script was written
for, not one it reads from the jar. When the Official emulator workflow records against another
release (`tools/oracle/README.md`), the header still names the pinned one. A new official release
therefore changes the constant in every script and `.github/actions/official-emulator/pinned.sh`
together, and every recording is made again ([OBSERVATION_POLICY.md](OBSERVATION_POLICY.md)).

Where the client that made a recording matters, the recording or its script says which:

- Go, Python and Java transcripts: the SDK versions pinned in `tools/oracle/sdk_go/go.mod`
  (`cloud.google.com/go/firestore` 1.26.0), `tools/oracle/sdk_python/requirements.txt`
  (`google-cloud-firestore` 2.34.1) and `tools/oracle/sdk_java/pom.xml`
  (`com.google.cloud:google-cloud-firestore` 3.49.0).
- Flutter Web: `tools/oracle/flutter_web/pubspec.yaml` pins `cloud_firestore` and the web
  packages, and the transcript records the firebase-js-sdk version and the URLs it was loaded from.
- `sdk-rules-unit-testing` and `browser-rules`: the transcript starts with the versions of
  `firebase` and `@firebase/rules-unit-testing`; `js-sdk-browser` records the firebase-js-sdk commit
  its suite ran from.
- `functions-triggers`: `results/README.md` names the versions of firebase-tools,
  `firebase-functions` and `firebase-admin` it ran with; `tools/oracle/functions/` asks for
  version ranges only.
- The other `tools/oracle/sdk_*.mjs` scripts install the current npm packages when they are run;
  their transcripts do not name a version.
- The Python oracle scripts use the Python standard library and, where their docstring says so,
  `grpcurl`; `write_stream.mjs` uses `@grpc/grpc-js` with the repository's protos.

### Recordings without a header

The repository has named one official release only, v1.22.0 (jar SHA-256 `9b6498b7…`), from the
first oracle script (#100) to now: in `tools/oracle/`, the README's commands and the scripts'
headers name no other. A recording without a header was therefore made against v1.22.0, and
anyone can check it: for each commit that added or changed the file
(`git log -- <path>`), `git grep -o "cloud-firestore-emulator[- ]v[0-9.]*" <commit> -- tools/oracle`
names that release only.

[`docs/parity.md`](../parity.md) shows each recording's release in its Oracle column, "not stated"
where the file names none.

## The corpus manifest

The [Hidane Oracle Corpus](../oracle-corpus.md) manifest, [`docs/oracle-corpus.json`](../oracle-corpus.json),
puts the provenance of every recording in one file, fixtures and official transcripts alike.
`tools/oracle/corpus.py` generates it from the recordings and `tools/parity/table.py`, and CI
fails when it is stale or does not fit its schema. For each recording it gives:

| Field | Provenance it records |
|---|---|
| `recorder`, `client` | The script that made it, the official emulator's interfaces it used, and the packages it used with their versions: pinned by the recorder, stated by the recording, or not known (installed when the script ran) |
| `oracle` | The release, its jar's SHA-256 where known, and how the release is known: `header`, `file name`, or `git history`, with the `evidence` (the check described above, done by the script) |
| `recorded` | The dates of the first and the last commit that changed it |
| `sha256`, `bytes` | Its content, so a copy can be matched with the manifest; `digest` does the same for the whole corpus |
| `tests`, `levels`, `counted`, `compared`, `compared_with` | How hidane is checked against it |

So the recordings without a header have their release stated in the manifest, with how it was
established; `git log -- <path>` then gives the commits and pull requests that made each one.
