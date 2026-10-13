# Differential fuzzing

The oracle scripts (`tools/oracle/`) ask the official emulator questions someone thought of. The
fuzzer asks the ones nobody did: it generates REST requests from a seed, sends each case to the
official emulator and to hidane, and compares the answers. Python 3 standard library only; it
sends requests with the oracle's transport and normalises answers with the oracle's
normalisation (`tools/oracle/harness.py`'s `send` and `mask`), so a difference it reports is one
the recordings would show.

```sh
java -Duser.language=en -Duser.country=US -jar cloud-firestore-emulator-v1.22.0.jar --host 127.0.0.1 --port 8086 &
hidane --host 127.0.0.1 --port 8186 &
out=$(mktemp -d)
python3 -I tools/fuzz/fuzz.py run --official 127.0.0.1:8086 --hidane 127.0.0.1:8186 --seed 1 --cases 500 --out "$out"
```

`--hidane-build VERSION:COMMIT` says which hidane the run fuzzes (the version `hidane --version`
prints and the full commit it was built from, such as `--hidane-build "0.4.0:$(git rev-parse
HEAD)"`); the summary and the run's entry record it next to the official emulator's version and
SHA-256, and say `unknown` without it.

`hidane benchmark --official-download` caches the pinned jar (`~/.cache/hidane-bench`), as does
firebase-tools (`~/.cache/firebase/emulators`). Start the jar with an English locale, as above
(tools/oracle/README.md says why). The jar is run and observed only
([docs/clean-room/](../../docs/clean-room/README.md)).

## Cases

Case `n` of seed `s` is always the same case (`fuzz.py show --seed s --case n` prints it), and
runs in a project of its own, a fresh one each time, so cases are independent and every one can
be run again alone. Areas take turns (`--areas` picks some):

| Area | What a case does |
|---|---|
| `documents` | Writes a document of random values (every type; NaN, ±0, ±infinity, the largest and smallest integers, long and Unicode strings, bytes, references, geo points, vectors, nested maps and arrays) by PATCH, create, commit or with a generated ID, then reads it, reads it with a mask, lists, batch-gets, updates with a mask, deletes, lists collection IDs |
| `queries` | Writes a small collection (and a collection group), then runs queries (field, unary and nested `AND`/`OR` filters with every operator, `in`/`not-in`/`array-contains-any`, `__name__`, orderings, cursors, limits, offsets, projections, under a document) and aggregations (`count` with `upTo`, `sum`, `avg`) |
| `transactions` | Begins a transaction (read-write, read-only, at a read time, or with `newTransaction` on a read), reads, queries and aggregates in it, commits or rolls it back, uses it again, writes outside it to what it read, or names a transaction that does not exist |
| `batches` | Commits and `batchWrite`s of up to six writes, several to one document: sets, masked updates, deletes, transforms, preconditions |
| `transforms` | Field transforms (server time, increment, maximum, minimum, array union and removal) on stored values of every kind, nested and missing fields |
| `rules` | Loads generated Security Rules (`allow` statements with random conditions over `request`, `resource`, lookups and the path) through `PUT …:securityRules`, then reads and writes as no one, users with unsigned tokens, a user with a custom claim, and the owner |
| `malformed` | Bad values (unknown types, wrong JSON types, out-of-range numbers and coordinates, bad base64, nested arrays, reserved and empty keys, bad vectors, too deep, too long), bad paths and names, bodies that do not parse, bad masks, queries and aggregations, oversized documents and commits, bad transforms |

The generators leave out the requests the official emulator never answers
([docs/parity-exceptions.md](../../docs/parity-exceptions.md)): `GET` with `?transaction=`, query
parameters that do not parse, a `parent` in a `:listCollectionIds` body, `GET //`.

## Differences

`diff.py` finds the first step whose answers differ, after normalising both: the oracle's
normalisation (project ID, server times, transaction IDs, page tokens, generated IDs), the values
the run captured (transaction IDs, page tokens and times named by later steps), timestamps of the
years 2021 to 2099 (the generators never write one, so these are the servers'), and versions of
those years in microseconds (precondition messages). It classifies the difference:

| Kind | |
|---|---|
| `crash` | one side answered 5xx or nothing (a timeout, a closed connection), the other did not |
| `status` | different HTTP statuses, or the same one with a different error status |
| `message` | the same error, worded differently |
| `order` | the same values in another order: list elements, or the fields of an object |
| `data` | anything else: values, how JSON is written, the content type |

Its signature (`message-commit-0123456789ab`) is the kind, the request (`:commit`,
`GET document`), both statuses and where or how the answers differ, without the inputs' values:
messages are compared as templates (`Document "…" has N […]`), a denial as the kinds of reasons
it lists, a status difference with the templates of the errors either side gave (refusing an
untyped value and refusing nested transforms are two differences, even of `:commit` with the same
statuses), a data difference as a path such as `document.fields.*.doubleValue`. Other seeds and
other areas that hit the same problem get the same signature, which is what the summary, the
artifacts and the issues deduplicate on.

### Accepted differences

[`exceptions.json`](exceptions.json) holds rules for the differences docs/parity-exceptions.md
accepts. Each rule names the entry of
[`docs/parity-exceptions.json`](../../docs/parity-exceptions.json) that accepts what it matches
(`exception`: `D008`, …), says `why`, and matches on the kind, the area, the request (`verbs`),
where the answers differ (`detail`), each side's status, error code, message and differing
`value` (regular expressions), and a `relation` between the answers (`hidane-reasons-subset`: a
denial listing some of the official one's reasons and nothing else, the official emulator's two
passes). A difference a rule matches is counted under its entry and not reported. `fuzz.py
check` fails when a rule names an entry that is gone or is malformed; keep the rules as narrow as
the entry's text.

Answers alone are often not narrow enough: an official `200` against hidane's `Payload isn't
valid for request.` is a lone surrogate (D083), which hidane cannot store, but until #280 it
was also a number given for a string, which was a bug. A rule can then say what the request
was:

| Key | Matches |
|---|---|
| `request.method`, `request.path` | the method and path of the step whose answers differ (regular expressions; the path as the case gives it, with `{project}` and the other placeholders, and its query string) |
| `request.headers` | `{"Content-Type": "(?i)^text/"}`: a regular expression per header, names in any case; a header the step does not set is the empty string |
| `request.predicates` | names of checks on the request (`PREDICATES` in `diff.py`), each of which must hold: `lone-surrogate` (a string of the body, key or value, holds an unpaired surrogate), `lenient-timestamp` (a string value of the body or a query parameter is shaped like a timestamp but is not RFC 3339: month 13, `2020-1-01`), `long-request-line` (the request line is longer than 4096 bytes), `read-write-transaction` (the body begins one with `newTransaction`) |
| `earlier` | the same keys for some step before that one, and the `status` of its answer (a number or a list; both sides answered it alike, since the first difference comes later) |

```json
{
 "id": "failed-query-lock",
 "exception": "D085",
 "why": "…",
 "official": {"status": 409, "code": "^ABORTED$", "message": "^Transaction lock timeout\\.$"},
 "hidane": {"status": 200},
 "earlier": {"path": ":run(Aggregation)?Query$", "predicates": ["read-write-transaction"], "status": 400}
}
```

A rule with `request` or `earlier` matches only with the case at hand, and the run records each
difference under its signature and the rule that accepts it: one signature can be accepted in
one case and unexplained in another, and is then two entries of the summary. A new predicate is
a function of a step in `diff.py`, with a test in `test_fuzz.py`.

### Minimising

A new difference is run again first: one that does not show twice is reported as not
reproduced (timing, the lookups docs/parity-exceptions.md calls unpredictable) and goes no
further. Then `shrink.py` keeps only what it needs, checking each candidate on both emulators
for the same signature, still not accepted by a rule (a smaller case can keep the signature and
become one `exceptions.json` accepts, such as a denial whose reasons are now a subset of the
official one's): the steps after the one that differs go, ddmin removes the earlier steps
it does not need, then each body is reduced top down (list elements by ddmin, object keys one at
a time, long strings halved), repeated while a pass removes something (a smaller body can make an
earlier step unneeded). `--minimize-budget` and `--minimize-seconds` bound it.

## Output

Everything goes to `--out`, which must be outside the repository: nothing the fuzzer finds is
committed by it.

- `differences/<signature>.json`: one per unexplained difference. `case` is the minimised case
  (`fuzz.py replay --official … --hidane … <file>` runs it again), `original` the case as found;
  `cases` holds its steps in the format of `tools/oracle/rest.py`'s fixture, with the official
  emulator's answers as `outcome`, and `hidane_outcomes` hidane's answers, next to the
  `oracle` and `generated_by` fields the recordings carry.
- `summary.json` and `summary.md`: the distinct differences (unexplained, accepted by each
  entry, not reproduced) with how often each was seen; per area, the cases run, those that
  differed and how many of those were accepted, unexplained or not reproduced; per kind, the
  distinct unexplained differences.
- `run.json`: the run as an entry of `results/differential-fuzzing.json`, the shape
  docs/compatibility.schema.json gives `fuzzing_run` (`cases`; `differences`, the distinct
  ones; `unexplained`; `hidane`, the build fuzzed), with the seed, the counts per area and per kind, and the distinct
  differences accepted by each entry. `fuzz.py record <out>/run.json` adds it there, for a run
  someone has looked at; `tools/parity/compatibility.py` reports it.

`run` exits 0 when it ran: differences are results, not failures. It stops early, and fails,
when an emulator stops answering.

### From a difference to a fixture

A difference file is input for a person, not a fixture:

1. Run it again (`fuzz.py replay`) and read both answers. Decide which side is right: the
   official emulator is the oracle, but some of its answers are bugs hidane does not reproduce.
2. If hidane is wrong, add the minimised request to the recorder of its area (`rest.py`,
   `document_writes.py`, `queries.py`, `transforms.py`, `same_document_writes.py`, …) and record
   the fixture again against the pinned jar, as tools/oracle/README.md says; the test that
   replays the fixture then fails until hidane is fixed. Steps in `cases` that name `{tx}`,
   `{token}`, `{commitTime}` or `{updateTime}` need a recorder that carries the value over,
   such as `harness.case`.
3. If hidane is right on purpose, add the difference to docs/parity-exceptions.md (and
   `docs/parity-exceptions.json`) and a rule here that names the new entry.

## Continuous verification

The Differential fuzzing workflow (`.github/workflows/fuzz.yml`) runs on GitHub-hosted runners
only, daily, by hand (`seed`, `cases`, `minutes`, and `issues` to file issues from a manual run
of main), and for changes to the fuzzer, `tools/oracle/harness.py` or the workflow (seed 1, a
few minutes):

- **fuzz** runs the tests above, builds hidane from the commit, starts it and the pinned jar
  (`.github/actions/official-emulator`), and fuzzes for the time given (25 minutes on the
  schedule, with a seed from the date), naming the build with `--hidane-build` (the version
  `target/release/hidane --version` prints and `GITHUB_SHA`). `summary.md` goes to the job summary; the run's
  `--out` and both emulators' output go to the `fuzz` artifact.
- **issues**, on main only, and the only job with `issues: write`: by hand with `issues` on, or
  on the schedule once the repository variable `FUZZ_FILE_ISSUES` is `true` (see below).
  [`issues.py`](issues.py) opens one issue per unexplained difference, labelled
  `fuzz`, whose first line is a marker naming the signature
  (`<!-- hidane-fuzz signature="…" first-run="…" -->`). A later run that finds the signature
  again updates the open issue and leaves a closed one closed: close it once hidane is fixed or
  the difference is accepted (with a rule here). At most 10 issues are opened per run, the most
  severe first (crash, data, status, order, message). An issue holds only fields checked against
  strict patterns and the error messages escaped in a code block; the bodies are in the
  artifact. The Upstream workflow's issues carry the label `upstream` instead.
- **issues (dry run)**, everywhere else, says what `issues` would do, with read-only access.

Scheduled runs do not file issues yet. The differences known so far are triaged in one tracking
issue ([#262](https://github.com/hidane-dev/hidane/issues/262)), so that the daily run does not
open one issue per signature for them. Once that list is worked down, set the repository
variable `FUZZ_FILE_ISSUES` to `true` (Settings, Secrets and variables, Actions, Variables) and
the schedule files issues as described above; unset or anything else, it only does the dry run.
A manual run with `issues` on files them either way.

## Limits

REST only: gRPC answers with the same codes and messages (the recordings check that), but the
Listen and Write streams, WebChannel and the WebSocket feed are not fuzzed. Requests run one at
a time, so lock waits are covered (a write outside a transaction to what it read) but not races.
Server-chosen times are masked by year, and the order of masked or transformed fields (a Java
`HashMap`'s) is left to `tools/oracle/field_order.py`; such differences tend to show as not
reproduced.

## Tests

```sh
python3 -m unittest discover -s tools/fuzz
```

They need neither emulator: the end-to-end test runs `fuzz.py run` against two small HTTP
servers that stand in for them.
