# Rules evaluator spike (#61)

A throwaway spike that ran one test set through four ways of evaluating Security Rules
conditions, to decide [ADR 0005](../../docs/adr/0005-rules-engine.md). It is a standalone cargo
project with its own `[workspace]`: the main workspace, CI and releases do not build it. Nothing
here is meant to be reused as is.

| Evaluator | What it is | Code |
|---|---|---|
| A | Hand-written lexer, parser, static checks and tree-walking interpreter with Rules semantics | `src/a/` |
| B | cel-rust (`cel` 0.15.0) as is: its CEL parser and interpreter, an environment without CEL's standard functions, the Rules library as context functions, Rules types (path, set, map diff, latlng) as custom `Val`s | `src/b.rs` |
| C | A's parser and static checks, translated to cel-rust's AST and run by cel-rust's interpreter with B's library (ADR 0005 option C) | `src/c.rs` |
| stock | cel-rust's standard environment, unextended (reference) | `src/b.rs` (`run_stock`) |

## Test set

- `cases/cel-spec.json`: 580 cases from the cel-spec conformance tests
  ([google/cel-spec](https://github.com/google/cel-spec) `tests/simple/testdata`, tag v0.25.3,
  Apache-2.0), files `basic`, `integer_math`, `fp_math`, `comparisons`, `logic`, `string`,
  `lists`, `conversions`, `fields`, `parse`, `plumbing`. `extract_cel_spec.py` reads the
  textproto files and keeps the cases whose expressions only use what Rules has (no uint,
  protobuf messages, type values, macros, optionals); the skip counts are in the file.
- `cases/rules.json`: 242 Rules conditions (`is`, missing fields and errors, maps, lists and
  sets, strings, numbers, timestamps, paths, latlng / bytes / hashing, lexical rules). Written
  while the evaluators were written, so they were tuned against these.
- `cases/rules-heldout.json`: 97 conditions recorded after the evaluators were written and not
  used to change them.
- `../../results/rules-conditions-official-v1.22.0.json`: what the official emulator decided for
  every condition, recorded with `tools/oracle/rules_conditions.py`. Each cel-spec case is sent
  as a check that it has the value cel-spec expects (`conditions.py`).

Two views:

- **CEL view**: each cel-spec expression against cel-spec's expected value.
- **Rules view**: each condition against the official emulator's outcome (allow, false, error,
  compile error). For a cel-spec case the evaluator runs the expression and its value is
  compared the way the check does.

## Run

```sh
cd spikes/rules-eval
cargo run --release                        # the tables below
cargo run --release -- failures c          # what C fails, with its result
```

To record again, with the official emulator jar running on 127.0.0.1:8086 under an English
locale (`java -Duser.language=en -Duser.country=US -jar …`; under another locale its warnings
and evaluation errors turn into 500s):

```sh
python3 -I extract_cel_spec.py <cel-spec>/tests/simple/testdata > cases/cel-spec.json
python3 -I conditions.py > /tmp/conditions.json
python3 -I ../../tools/oracle/rules_conditions.py 127.0.0.1:8086 /tmp/conditions.json \
  > ../../results/rules-conditions-official-v1.22.0.json
```

`src/bin/size_*.rs` build A alone, B alone and neither, to compare binary sizes.

## Results (2026-10-11)

CEL view: cel-spec expressions against cel-spec's expected values

| Group | Cases | A | B | C | stock |
|---|---:|---:|---:|---:|---:|
| basic | 33 | 22 | 33 | 22 | 33 |
| comparisons | 159 | 129 | 159 | 142 | 159 |
| conversions | 32 | 15 | 15 | 15 | 32 |
| fields | 16 | 8 | 16 | 8 | 16 |
| fp_math | 30 | 26 | 30 | 27 | 30 |
| integer_math | 42 | 42 | 42 | 42 | 42 |
| lists | 19 | 9 | 15 | 9 | 19 |
| logic | 30 | 25 | 30 | 25 | 30 |
| parse | 163 | 58 | 161 | 58 | 163 |
| plumbing | 5 | 5 | 5 | 5 | 5 |
| string | 51 | 13 | 17 | 13 | 51 |
| **total** | 580 | 352 | 523 | 366 | 580 |

Rules view: conditions against the official emulator's outcome (0 internal errors left out)

| Group | Cases | A | B | C | stock |
|---|---:|---:|---:|---:|---:|
| cel-spec: basic | 33 | 29 | 24 | 29 | 24 |
| cel-spec: comparisons | 158 | 147 | 128 | 134 | 128 |
| cel-spec: conversions | 32 | 16 | 30 | 16 | 14 |
| cel-spec: fields | 16 | 3 | 7 | 3 | 7 |
| cel-spec: fp_math | 30 | 30 | 26 | 29 | 26 |
| cel-spec: integer_math | 42 | 41 | 41 | 41 | 41 |
| cel-spec: lists | 19 | 9 | 13 | 9 | 9 |
| cel-spec: logic | 30 | 21 | 26 | 21 | 26 |
| cel-spec: parse | 163 | 155 | 65 | 155 | 63 |
| cel-spec: plumbing | 4 | 4 | 4 | 4 | 4 |
| cel-spec: string | 51 | 18 | 47 | 18 | 13 |
| held out: error absorption | 36 | 35 | 35 | 35 | 35 |
| held out: mixed | 70 | 70 | 56 | 63 | 27 |
| rules: is | 17 | 17 | 0 | 17 | 0 |
| rules: latlng, bytes, hashing | 6 | 6 | 6 | 6 | 2 |
| rules: lexical and static checks | 86 | 82 | 71 | 80 | 71 |
| rules: lists and sets | 17 | 17 | 15 | 16 | 2 |
| rules: maps | 20 | 20 | 19 | 19 | 6 |
| rules: missing fields and errors | 22 | 22 | 22 | 22 | 22 |
| rules: numbers | 25 | 23 | 22 | 20 | 15 |
| rules: paths | 12 | 12 | 7 | 12 | 4 |
| rules: strings | 20 | 20 | 18 | 19 | 8 |
| rules: timestamps and durations | 17 | 17 | 17 | 17 | 1 |
| **total** | 926 | 814 | 699 | 785 | 548 |

Lines of code (non-blank, non-comment, after `cargo fmt`): A 1,405 (lexer and parser 484,
static checks 167, values and interpreter 351, library 342, glue 61); B 692 (custom types 96,
environment and inputs 115, accessors over `dyn Val` 79, library 402); C 139 on top of A's
parser and checks and B's types and library.

Binary size, stripped release builds on macOS arm64 (`src/bin/size_*.rs`): neither 385 KiB, A
1.87 MiB, B 3.26 MiB. Most of A's share is the `regex` crate, which B has too; cel-rust and its
own dependencies add 1.4 MiB on top. `cel` 0.15.0 pulls in 41 crates (antlr4rust, nom,
chrono-tz among them); `cel`, `chrono-tz`, `antlr4rust` and `nom` take about 14 s of compile
time in a release build.

What the numbers mean is in ADR 0005.
