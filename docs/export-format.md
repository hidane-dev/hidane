# Export format and import

How the official Cloud Firestore emulator (v1.22.0) exports and imports data, and how hidane
reproduces it. Everything here was observed from the outside: exports written by the official
emulator, read byte by byte, and its answers to imports of exports made for the purpose. Nothing
was decompiled. `tools/oracle/export_import.py` records the observations in
`crates/hidane/tests/fixtures/export_import.json`, and `crates/hidane/tests/export_import.rs`
replays them against hidane. The format is Cloud Datastore's managed export: LevelDB log files
holding App Engine `EntityProto`s (#88).

## How firebase-tools uses it

- `firebase emulators:start --export-on-exit <dir>` and `firebase emulators:export <dir>` call
  `POST /emulator/v1/projects/{project}:export` with
  `{"database": "projects/{project}/databases/(default)", "export_directory": <a temporary directory>, "export_name": "firestore_export"}`,
  then move the directory to `<dir>` and write `<dir>/firebase-export-metadata.json` next to it.
  Only the `(default)` database is exported.
- `firebase emulators:start --import <dir>` reads `firebase-export-metadata.json` and starts the
  emulator with `--seed_from_export <dir>/firestore_export/firestore_export.overall_export_metadata`.
- The emulator's own `--export-on-exit` and `--export-name` flags are never passed. They export
  nothing in Firestore Native mode (the official emulator was stopped with `SIGINT` after a write:
  the directory stayed empty).

## Files

An export named `N` written to `D` is the directory `D/N/`:

```
D/N/N.overall_export_metadata
D/N/all_namespaces/all_kinds/all_namespaces_all_kinds.export_metadata
D/N/all_namespaces/all_kinds/output-0
```

An export of an empty database is `N.overall_export_metadata` alone. The emulator always writes
one kind directory and one output file, however large the export (a 120 MB export of 620
documents was one `output-0`).

### LevelDB log

`output-*` and `.overall_export_metadata` are [LevelDB logs](https://github.com/google/leveldb/blob/main/doc/log_format.md):
32 KiB blocks of records. A record is written as one or more fragments, each behind a 7-byte
header:

| Bytes | Content |
|---|---|
| 0–3 | CRC-32C (Castagnoli) of the type byte and the data, masked: `((crc >> 15) \| (crc << 17)) + 0xa282ead8`, little endian |
| 4–5 | data length, little endian |
| 6 | type: `1` FULL, `2` FIRST, `3` MIDDLE, `4` LAST |

A fragment never crosses a block boundary. When fewer than 7 bytes are left in a block, they are
zeros and the next fragment starts the next block. A 70,000-byte record is FIRST, MIDDLE and LAST
over three blocks; hidane's file for it has the official file's length and CRC.

### `N.overall_export_metadata`

A log of two records:

1. one byte, `0x33`, the format version;
2. an `OverallExportMetadata` message, with one field 1 per kind directory (none for an empty
   export):

| Field | Content |
|---|---|
| 1.1 | `{1: 2, 3: 3}`, the same in every export |
| 1.2 | the kind metadata's path, relative to `D/N/` |
| 1.3 | the number of entities |
| 1.4 | the size of the kind's output files in bytes |

### `all_namespaces_all_kinds.export_metadata`

A plain protobuf message (not a log): field 1 holds `{1: N, 2: start time, 3: end time}` (times
in microseconds since the epoch), field 2 holds `{1: "", 2: "output-0"}`, one per output file,
relative to the kind directory.

### Entities

`output-0` holds one `EntityProto` per document, in no particular order:

| Field | Content |
|---|---|
| 13 `key` | `{13 app: "dev~{project}", 14 path: {1 element (group): {2 type: collection ID, 3 id \| 4 name}}…, 23 database_id}`. A numeric document ID (`__id7__`, also negative ones) is an `id`, any other ID a `name`. `database_id` is left out for `(default)` |
| 14 `property` | Empty arrays only, before every other property |
| 15 `raw_property` | Every other field, in the order the document's fields were written |
| 16 `entity_group` | The path's first element only |

Documents that exist only as the parent of other documents are not exported.

A property is `{1 meaning, 3 name, 4 multiple, 5 value}` (meaning left out when 0). An array is
one property per element, each marked `multiple`; an empty array is a property 14 of meaning 24
with an empty value. Values:

| Firestore value | Meaning | `PropertyValue` |
|---|---|---|
| null | | empty |
| boolean | | 2 |
| integer | | 1 |
| double | | 4 (fixed64); `-0.0` is written as `0.0` |
| timestamp | 7 | 1: microseconds since the epoch |
| string | | 3 |
| bytes | 14 | 3 |
| reference | | 12 (group): `{13 app: "dev~{project}", 14 element (group): {15 type, 16 id \| 17 name}…, 23 database_id}` |
| geo point | 9 | 5 (group): `{6 latitude, 7 longitude}` |
| map | 19 | 3: an entity of its own: key `{13: "", 14: ""}`, the map's fields as properties (empty arrays first), entity group `""` |
| vector | 19 | 3: an entity like a map's, whose properties are the elements, `multiple`, each named `__vector__` |

## Import

`:import` and `--seed_from_export` read an export from its `.overall_export_metadata` file and
follow the pointers in it, so other layouts are read too. A managed export of a Native mode
database, as the oracle builds one (no real one was at hand), was read the same by the official
emulator and by hidane:

- a kind directory per collection (`all_namespaces/kind_users/…`) and several output files;
- the app `s~{project}`;
- top-level properties indexed (property 14) where Datastore allows it. A reference in a
  top-level indexed property, alone or in an array, is imported into the importing project,
  whatever project it named; references anywhere else keep their project. (An indexed property
  of meaning 14 fails the import with "A property with meaning BLOB cannot be indexed.")

What is written where:

- The export's project is not kept: documents go into the project being imported into.
  References keep the project they name, except as above, and are not checked.
- A database receives the documents whose key names its ID: an export of `(default)` imports into
  `(default)` databases only, an export of `db2` into `db2` databases only. Others are skipped.
- Documents already in the database stay, except those the import writes. A written document
  takes the import time as `update_time` and keeps its `create_time` if it existed; one that
  already holds exactly the imported fields is left as it is, `update_time` included.
- An import that fails writes nothing.

### `--seed_from_export`

The export is read at startup. A bad export stops the emulator with exit code 1:

| Seed | Message |
|---|---|
| missing, unreadable, a directory | `Failed to parse overall export metadata file` |
| not a log, or a first record other than `0x33` | `Overall export metadata file version not supported` |
| an output file missing | `Failed parse entity file:{path}` |

`--import-data` is the same flag under another name.

A database receives its documents on its first access, in one write at that time (so
`create_time` equals `update_time`, the access time). Every project is seeded: the export of one
project's `(default)` database seeds `(default)` in every project. Clearing a database
(`DELETE /emulator/v1/projects/{p}/databases/{d}/documents`) leaves it empty; after `POST /reset`
every database is seeded again on its next access.

## Endpoints

`POST /emulator/v1/projects/{project}:export` and `…:import`, also under
`/emulator/v1/projects/{project}/databases/{database}`. The project and database in the path are
not used. Other methods, a trailing slash or another verb answer `404 Not Found`. Success is
`200` with the body `{\n}\n`.

The body is read as protobuf-java reads JSON: both spellings of a key (`export_directory`,
`exportDirectory`), `null` for an empty value, a number or a boolean as its text. A body that is
not a JSON object, or a key the request does not have (`export_name` on `:import`), is
`400 INVALID_ARGUMENT` "Payload isn't valid for request." An empty body is an empty request. The
`Authorization` header is read as by the other endpoints (a malformed token is
`400 INVALID_ARGUMENT` "invalid jwt").

`:export` takes `{database, export_directory, export_name}`:

| Request | Answer |
|---|---|
| `database` not `projects/{p}/databases/{d}` | `400 INVALID_ARGUMENT`, the resource name message (`Database name "" lacks "projects" at index 0.`) |
| `export_directory` missing, relative to a missing directory, or a file | `400 FAILED_PRECONDITION` "export_directory must be a directory" |
| no `export_name` | named `firestore_export_{seconds since the epoch}` |
| `export_name` holding `/` | `500 INTERNAL` "failed to write export", after writing part of it |
| an existing name | overwritten |

`:import` takes `{database, export_directory}`, where `export_directory` names an
`.overall_export_metadata` file:

| Request | Answer |
|---|---|
| a bad `database` | as for `:export` |
| no file, a missing file, a directory | `400 INVALID_ARGUMENT` "Failed to parse overall export metadata file" |
| another version | "Overall export metadata file version not supported" |
| a kind metadata file missing | "`{path}` (No such file or directory)" |
| an output file missing | "Failed parse entity file:`{path}`" |
| a fragment whose checksum does not match | "Checksum doesn't validate." |
| a truncated output file | "Invalid record" |

## hidane

hidane writes the same files: the overall metadata byte for byte, each entity byte for byte, the
kind metadata with its own times. The official emulator imports hidane's exports into the same
documents as its own, and the reverse; with firebase-tools 15.33.0,
`hidane exec -- firebase emulators:exec --export-on-exit … --import …` round-trips through either
emulator. Differences, all without effect on what is imported:

- Entities are written in document name order, fields in name order (#108).
- A seed holding documents of another database leaves them out. The official emulator keeps them
  in the seeded database, where no read finds them, and writes them into that database's exports.
- `--export-on-exit` prints a warning that it exports nothing.
- A bad seed is reported as `ERROR: {message}` on stderr, where the official emulator logs a
  Java exception carrying the same message.
- The seed stays in memory, decoded, for the databases seeded later: with the 120 MB export
  seeded, hidane held 490 MiB.
