# ADR 0005: Security Rules evaluator approach

- **Status**: Accepted (option A, decided by the #61 spike)
- **Date**: 2026-10-11 (draft 2026-10-09)

## Context

The Rules language is a CEL-based expression language with a few syntactic extensions
(`match` / `allow` / `function` / `let`, `is`, `in`, `b'...'`, and the Path / Set / MapDiff /
Timestamp / Duration / LatLng types). No formal grammar is published. Hardest first:

1. `list` evaluation = deciding whether a query's constraints imply the rule condition. The
   official docs only say rules are not filters and that a query is rejected if it could return a
   document the rule would deny; the analysable expression forms are not specified
   ([rules-query](https://firebase.google.com/docs/firestore/security/rules-query))
2. `get` / `exists` / `getAfter` / `existsAfter`: coupling to storage, 10 / 20 call limits,
   cached calls not counted ([rules-conditions](https://firebase.google.com/docs/firestore/security/rules-conditions))
3. Type system and semantics: 14 types, 70+ methods; overflow, division by zero and
   missing-field errors and how they interact with `&&` / `||` are undocumented
4. Grammar: adequately documented; a hand-written parser is enough

CEL has official implementations in C++, Go and Java ([cel-spec](https://github.com/google/cel-spec)).
Linking one would bring a C++ toolchain, a Go runtime or a JVM into a single Rust binary, so they
are not options; their shared conformance tests are used below. The Rust candidate is cel-rust.

Oracles: the official emulator (differential tests) and the production Rules API `projects:test`,
which returns diagnostics and evaluation results without a database.
Known official-emulator bug #6252 is not reproduced (see [parity-exceptions.md](../parity-exceptions.md)).

## Options

### A. Hand-written parser + tree-walking interpreter (Rust)

- Pro: full control over Rules-specific types, methods, error semantics and diagnostics (`issues[]` with positions)
- Con: 70+ methods and conversions to write; CEL subtleties tracked by hand

### B. Reuse cel-rust for expressions, extend with Rules syntax and types

- Pro: CEL evaluation, types and error semantics for free
- Con: Rules types (Path, Set, MapDiff, Timestamp methods) and `is` / `in` semantics may not match; a fork if the extension points are insufficient

### C. A for grammar and types, B for expression evaluation only

### `list` implication (common to all options)

- Planned: a conservative subset (`==` `!=` `<` `<=` `>` `>=` `in` `or` `array-contains`
  `array-contains-any` combined with `resource.data.<field>` / `resource.id` / `request.auth.*`),
  undecidable expressions **denied**, widened by measuring the official emulator
- Outcome (#66): measuring showed the official emulator's own model, small enough to reproduce
  whole, so hidane implements it instead of a subset (see below)

## Spike results (#61)

One test set ran through four evaluators ([spikes/rules-eval](../../spikes/rules-eval/README.md),
not built by CI):

| Evaluator | What it is |
|---|---|
| A | Hand-written lexer, parser, static checks and tree-walking interpreter |
| B | cel-rust (`cel` 0.15.0): its parser and interpreter, with the Rules library and types added through its API |
| C | A's parser and checks, translated to cel-rust's AST and run by cel-rust's interpreter with B's library |
| stock | cel-rust's standard environment, unextended |

The test set:

- 580 cel-spec conformance cases (v0.25.3: `basic`, `integer_math`, `fp_math`, `comparisons`,
  `logic`, `string`, `lists`, `conversions`, `fields`, `parse`, `plumbing`), without uint,
  protobuf messages, type values, macros or optionals
- 242 Rules conditions written together with the evaluators, and 106 written afterwards that
  were not used to change them (held out)
- The official emulator's outcome (allow, false, error, compile error) for every condition,
  recorded by [`tools/oracle/rules_conditions.py`](../../tools/oracle/rules_conditions.py) in
  `results/rules-conditions-official-v1.22.0.json`; a cel-spec case is sent as a check of the
  value cel-spec expects. The jar runs with an English JVM locale: its messages for warnings and
  evaluation errors exist in English only, and under another locale every warning or evaluation
  error becomes a 500 `UNKNOWN`. The first recording, under `ja_JP`, had 70 such 500s and 188
  false compile errors; the tables and findings below are from the corrected one

| | A | B | C | stock |
|---|---:|---:|---:|---:|
| Matches the official emulator, held-out conditions (106) | **105** | 91 | 98 | 62 |
| Matches the official emulator, the rest (820) | 709 | 608 | 687 | 486 |
| Matches cel-spec's expected values (580) | 352 | 523 | 366 | 580 |
| Code written (non-blank lines) | 1,405 | 692 | 139, plus A's parser and checks and B's library | 0 |
| Stripped binary, macOS arm64 (no evaluator: 0.4 MiB) | 1.9 MiB | 3.3 MiB | about B | |

A was tuned against the rest while it was recorded under the wrong locale, so it rejects some
conditions the official emulator accepts with a warning; it still matches the most.
A and C score lower against cel-spec because they follow the official emulator where it differs
from CEL. Per-group tables are in the spike's README.

### What the official emulator does

It is the parity target, and it is not CEL:

| Finding | Evidence (`results/rules-conditions-official-v1.22.0.json`) |
|---|---|
| 130 of the 578 cel-spec cases do not compile, all for lexical reasons: no exponents or hex in numbers; no raw or triple-quoted strings; no `\a` `\v` `\?` `` \` `` `\e` `\X` `\U` escapes; no raw control characters in a string; `--` is a token | `cel/…` with `outcome: compile` |
| A function or method that does not exist (CEL's `size(x)`, `startsWith`, `endsWith`, `contains`, `bool()`, any unknown `f()`) compiles with a warning, `Invalid function name: f.`, and fails when evaluated. An unused function is a warning too | `cel/…` with `outcome: error`; `:securityRules` answers `200` with `issues[]` |
| Types are checked when the condition runs, not before. `'1' == 1` and `null == false` are false; `[1] + [2]`, `true < false`, `7 in {}` and a map literal with int keys are errors; `false && 32` is false. The one static check that fails a compilation: a regex literal that does not compile. The compiler's type checker only warns (#60) | `rules/…`, `cel/…`, `heldout/…` |
| 6 cel-spec cases evaluate differently: `matches` is a full match, and `[1] == [1.0]` is false although `1 == 1.0` is true. `1 in [1.0]` is false too, and `int()` of an out-of-range double has no range error | `cel/string/matches/…`, `rules/…` |
| Rules adds `is`, path literals with `$( )`, slices `[i:j]`, string indexing and mixed int / float arithmetic | `rules/…` |
| Errors are absorbed the CEL way, whatever they come from: `error \|\| true` and `true \|\| error` allow, `error && false` and `false && error` are false, `error \|\| false` denies. Missing fields, a null `request.auth`, `request.path[10]`, `int('abc')`, division by zero and integer overflow all behave so | `heldout/…` (error absorption), `rules/…` (missing fields and errors) |
| A denial lists each statement's result with its line: `false for 'get' @ L5`, or the error, `Type conversion error. Argument: [abc], From type: [string], To type: [int]. for 'get' @ L5`. The first of its two passes (#6252) adds its own: the same error again, or `evaluation error at L5:22 for 'get' @ L5` (the condition's start) where `resource.data` or a lookup failed | a `PERMISSION_DENIED` body; `crates/hidane/tests/fixtures/rules_denials.json` |

### cel-rust

| Question | Answer |
|---|---|
| Version, licence, MSRV | `cel` 0.15.0 (2026-10-06), MIT, rust-version 1.86 (hidane: 1.88). `cel-interpreter` and `cel-parser` are the names before 0.11; last released 2025-07 |
| Maintenance | [cel-rust/cel-rust](https://github.com/cel-rust/cel-rust) (formerly clarkmcc/cel-rust): eight releases and over 300 commits (merges included) in 2026, two main maintainers. 0.13, 0.14 and 0.15 each list several breaking changes (the value model moved to `dyn Val` and `CowVal`) |
| Conformance | Runs the cel-spec v0.25.3 suite in CI; the ignored cases are mostly protobuf, dynamic types and extensions. Stock cel-rust passed all 580 cases here |
| What can be added | Custom types (the `Val` trait and capabilities such as `Indexer`, `Container`), functions (`Context::add_function` untyped, `Env::add_overload` typed), macros, type names |
| What cannot, without a fork | The grammar (ANTLR-generated: no `is`, path literals or slices); the operators (`+ - * / % == < in []` and `&&` / `\|\|` / `?:` are hard-coded in the interpreter for built-in types); the standard library, all or nothing (changing `matches` means dropping it and registering every function) |
| Error semantics | CEL's commutative `&&` / `\|\|`, built in, which is what the official emulator does |
| Diagnostics | Parse errors with line and column, for CEL syntax only; runtime errors carry no position; no type checker; no evaluation hooks (`:ruleCoverage` needs them) |
| Cost | 41 crates (`antlr4rust`, `nom`, `chrono-tz` among them), 1.4 MiB more than A in a stripped binary, about 14 s more crate compile time in a release build |

### Effort

- **A**: every mismatch found while writing it was a few lines in the lexer, the checker or the
  operators. Its one held-out miss is the static check of a regex literal.
- **B**: its Rules library is longer than A's (402 lines against 342) for the same functions:
  arguments arrive as `dyn Val` and are downcast one at a time, values passed to a function
  borrow the expression or the variables and need a `'static` copy to go into a set, and a
  namespaced function (`math.abs`) receives only its short name. It cannot parse `is`, paths or
  slices, accepts CEL syntax that Rules rejects and has no static checks.
- **C**: as much code as A, since it needs A's front end, and it still fails wherever cel-rust's
  operators differ from Rules: mixed arithmetic, strict equality inside lists and maps,
  membership, string indexing and slices. Fixing those means forking the interpreter.

## Decision

**Option A**: a hand-written lexer, parser, static checker and tree-walking interpreter, with no
dependency on cel-rust.

Rules is a dialect of CEL, not CEL. Its grammar and lexical rules are narrower in some places
and wider in others, and its library and several operators differ; its error semantics are
CEL's. cel-rust's parser, operators and standard library are the parts that would have to be
replaced, and the grammar and the operators cannot be replaced without a fork.
What would stay, the value model and overload dispatch, costs more code than it saves (B's
library is longer than A's) and brings 41 crates, 1.4 MiB and a 0.x API that breaks every few
months. A matched more of the held-out set than C (105 of 106 against 98), and its miss is cheap to
fix.

cel-spec's conformance tests stay in use: the cases the official emulator agrees with test the
CEL part, and the ones it rejects or evaluates differently are kept as Rules expectations.
Golden results come from the official emulator (`tools/oracle/rules_conditions.py`) and, later,
the production Rules API `projects:test`.

## `list` as the official emulator decides it (#66)

Measured with [`tools/oracle/rules_list.py`](../../tools/oracle/rules_list.py) (the official
emulator's `debug()` output shows its representation):

- The query's filter is split into the ways a document may satisfy it: each `or` branch and each
  value of an `in` or `array-contains-any`. A way with the same filters as an earlier one, in
  any order, is evaluated once. Every way must be allowed; the first way denied gives the
  denial.
- In each way `resource` is `{data: …}` with only the fields the filter names. Each filter of a
  field gives comparator and value pairs (`GT 0`, `NEQ 3`, `LIST_CONTAINS 'a'`), each kept once.
  A field left with one `EQ` (one `==`, `in`, `is null` or `is nan`, or `== 1` twice) holds that
  value, and the condition evaluates as usual (`resource.data.n + 1 == 2` works). Any other
  field holds a `constraint` of the pairs.
- Null and NaN (#167): `is not null` is `NEQ null` and `is not nan` `NEQ NaN`. A field filter
  that compares with null or NaN matches no document (`n != null`, `n == NaN`, `n > null`,
  `in [null]`), and is `EQ true, EQ false`, a pair no value satisfies; `!= NaN` matches every
  value but null and is `NEQ null`; `not-in` gives each value's pair, and `array-contains` null
  or NaN is `LIST_CONTAINS []`. The coverage report and `debug()` show these pairs.
- A comparison with a constraint is true when one of its filters alone implies it, else an
  error without a message (`evaluation error at L5:22`), never false. So `n >= 1 && n <= 1` does
  not imply `n == 1`, and a float in a filter does not compare with an int in the rule (but an
  `EQ` of a float implies `!=` any int). `!= null` always holds; `false` is less than `true`.
  NaN compares as in IEEE 754: `EQ NaN` lets nothing through and implies every comparison with a
  number, and any other number implies `!= NaN`. `<`, `<=`, `>` and `>=` with a bool, null, a
  list, a map, a set, a latlng or a path are a type error. `x in c` and `c.hasAny(list)` read
  `LIST_CONTAINS`; `c is constraint` is true.
- What follows for decisions: a filter that matches nothing implies `== true` and `== false`
  (through `EQ true, EQ false`), so the official emulator allows its query under conditions
  such as `resource.data.n == true`; the query returns no document. `is nan` next to another
  filter of the same field (`n is nan && n != 'a'`) implies any comparison with a number,
  although the query returns the documents whose field is NaN, which `resource.data.n > 0`
  would deny one by one. hidane decides both as the official emulator does.
- A way with `__name__ ==` (or one name of `__name__ in`) is decided on that document as stored,
  ignoring the rest of the filter. Ordering, cursors and limits constrain nothing.
- The list is checked on the collection's path and an unknown document ID (a wildcard there is
  unbound); a collection group adds an unknown run of segments before its collection ID, which
  only a recursive wildcard takes, and its `request.path` cannot be read.

## Consequences

- #59 (parser and diagnostics) follows the spike's lexer and parser: Rules lexical rules as
  measured, byte offsets on every node, `--` / `++` tokens, path literals, `is`, slices
- #60 (types and built-ins) checks types when a condition runs. At compile time it reports
  regex literals that do not compile (an error), and unknown or unused functions and the
  official type checker's findings (warnings), as the official emulator does: each expression
  has the types of what it is (an operator's or a function's results, a wildcard's), each
  operand, receiver and argument is expected to have some types, parameters and `let`s take the
  types their uses expect, and each call of a ruleset function checks its arguments against
  them. Measured with [`tools/oracle/rules_warnings.py`](../../tools/oracle/rules_warnings.py)
  (`crates/hidane-rules/src/check.rs` describes the rules)
- #62: errors are absorbed the CEL way, as on the official emulator, and every condition is
  evaluated once (#6252 is not reproduced)
- `spikes/rules-eval/cases/` and `results/rules-conditions-official-v1.22.0.json` become fixtures
  for the evaluator's tests; the spike code is a reference, not a starting crate
- Dependencies for the evaluator: `regex` for `matches` / `split` / `replace` and a SHA-2 crate
  for `hashing`
- Storage (ADR 0002) must expose a "state after this batch" projection for `getAfter`
- Revisit if cel-rust gains a replaceable grammar and operator layer, or if Rules moves closer
  to CEL
