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

- v0.2 ships a conservative subset (`==` `!=` `<` `<=` `>` `>=` `in` `or` `array-contains`
  `array-contains-any` combined with `resource.data.<field>` / `resource.id` / `request.auth.*`).
  Undecidable expressions are **denied** with a logged reason
- The subset is widened by measuring the official emulator

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
- 242 Rules conditions written together with the evaluators, and 97 written afterwards that were
  not used to change them (held out)
- The official emulator's outcome (allow, false, error, compile error) for every condition,
  recorded by [`tools/oracle/rules_conditions.py`](../../tools/oracle/rules_conditions.py) in
  `results/rules-conditions-official-v1.22.0.json`; a cel-spec case is sent as a check of the
  value cel-spec expects. 55 conditions it answered with a 500 are left out

| | A | B | C | stock |
|---|---:|---:|---:|---:|
| Matches the official emulator, held-out conditions (97) | **88** | 74 | 81 | 45 |
| Matches the official emulator, the rest (774) | 774 | 495 | 765 | 421 |
| Matches cel-spec's expected values (580) | 352 | 523 | 366 | 580 |
| Code written (non-blank lines) | 1,405 | 692 | 139, plus A's parser and checks and B's library | 0 |
| Stripped binary, macOS arm64 (no evaluator: 0.4 MiB) | 1.9 MiB | 3.3 MiB | about B | |

A was tuned against the 774, so its score there only says the measured behaviour fits in it.
A and C score lower against cel-spec because they follow the official emulator where it differs
from CEL. Per-group tables are in the spike's README.

### What the official emulator does

It is the parity target, and it is not CEL:

| Finding | Evidence (`results/rules-conditions-official-v1.22.0.json`) |
|---|---|
| 230 of the 578 cel-spec cases do not compile, for the reasons in this row and the next. No exponents or hex in numbers; no raw or triple-quoted strings; no `\a` `\v` `\X` `\U` escapes; no raw backspace or form feed in a string; `--` is a token; CEL's `size(x)`, `startsWith`, `endsWith`, `contains` and `bool()` do not exist | `cel/…` with `outcome: compile` |
| A static type check runs before evaluation: `'1' == 1`, `null == false`, `true ? 'a' : 1`, `false && 32`, `[1] + [2]`, `resource.data.arr + [4]`, `string(b'')`, a map literal with an int key, a regex literal that does not compile | `rules/…`, `cel/…` with `outcome: compile` (status 500, no message) |
| 10 cel-spec cases evaluate differently: `matches` is a full match, `[1] == [1.0]` is false although `1 == 1.0` is true, float `%` works, `int()` of an out-of-range double has no range error. `1 in [1.0]` is false too | `cel/string/matches/…`, `rules/…` |
| Rules adds `is`, path literals with `$( )`, slices `[i:j]`, string indexing and mixed int / float arithmetic | `rules/…` |
| An error from `resource` is absorbed by `\|\|` / `&&` only when the deciding operand does not read `resource`: `missing == 1 \|\| request.method == 'get'` allows, `missing == 1 \|\| resource.data.n == 1` is an error. CEL absorbs both | `heldout/…` (error absorption) |
| An error that does not come from `resource` (a null `request.auth`, `request.path[10]`, `int('abc')`, division by zero, literal overflow, ordering bools, `7 in {}`) ends the request with 500 `UNKNOWN`, even under `\|\| true` | `outcome: internal` |
| A denial reports the condition's start (`evaluation error at L5:22`), never the failing part | every `outcome: error` |

### cel-rust

| Question | Answer |
|---|---|
| Version, licence, MSRV | `cel` 0.15.0 (2026-10-06), MIT, rust-version 1.86 (hidane: 1.88). `cel-interpreter` and `cel-parser` are the names before 0.11; last released 2025-07 |
| Maintenance | [cel-rust/cel-rust](https://github.com/cel-rust/cel-rust) (formerly clarkmcc/cel-rust): eight releases and over 300 commits (merges included) in 2026, two main maintainers. 0.13, 0.14 and 0.15 each list several breaking changes (the value model moved to `dyn Val` and `CowVal`) |
| Conformance | Runs the cel-spec v0.25.3 suite in CI; the ignored cases are mostly protobuf, dynamic types and extensions. Stock cel-rust passed all 580 cases here |
| What can be added | Custom types (the `Val` trait and capabilities such as `Indexer`, `Container`), functions (`Context::add_function` untyped, `Env::add_overload` typed), macros, type names |
| What cannot, without a fork | The grammar (ANTLR-generated: no `is`, path literals or slices); the operators (`+ - * / % == < in []` and `&&` / `\|\|` / `?:` are hard-coded in the interpreter for built-in types); the standard library, all or nothing (changing `matches` means dropping it and registering every function) |
| Error semantics | CEL's commutative `&&` / `\|\|`, built in. Not what the official emulator does |
| Diagnostics | Parse errors with line and column, for CEL syntax only; runtime errors carry no position; no type checker; no evaluation hooks (`:ruleCoverage` needs them) |
| Cost | 41 crates (`antlr4rust`, `nom`, `chrono-tz` among them), 1.4 MiB more than A in a stripped binary, about 14 s more crate compile time in a release build |

### Effort

- **A**: every mismatch found while writing it was a few lines in the lexer, the checker or the
  operators. Its held-out misses are one rule, error absorption (8), and one static check, an
  invalid regex literal (1).
- **B**: its Rules library is longer than A's (402 lines against 342) for the same functions:
  arguments arrive as `dyn Val` and are downcast one at a time, values passed to a function
  borrow the expression or the variables and need a `'static` copy to go into a set, and a
  namespaced function (`math.abs`) receives only its short name. It cannot parse `is`, paths or
  slices, accepts CEL syntax that Rules rejects and has no static checks.
- **C**: as much code as A, since it needs A's front end, and it still fails wherever cel-rust's
  operators differ from Rules: mixed arithmetic, strict equality inside lists and maps,
  membership, string indexing, error absorption. Fixing those means forking the interpreter.

## Decision

**Option A**: a hand-written lexer, parser, static checker and tree-walking interpreter, with no
dependency on cel-rust.

Rules is a dialect of CEL, not CEL. Its grammar is narrower in some places and wider in others,
it type-checks before it runs, and its library, several operators and its error semantics
differ. cel-rust's parser, operators, standard library and error handling are the parts that
would have to be replaced, and the operators and `&&` / `||` cannot be replaced without a fork.
What would stay, the value model and overload dispatch, costs more code than it saves (B's
library is longer than A's) and brings 41 crates, 1.4 MiB and a 0.x API that breaks every few
months. A matched more of the held-out set than C (88 of 97 against 81), and its misses are cheap
to fix.

cel-spec's conformance tests stay in use: the cases the official emulator agrees with test the
CEL part, and the ones it rejects or evaluates differently are kept as Rules expectations.
Golden results come from the official emulator (`tools/oracle/rules_conditions.py`) and, later,
the production Rules API `projects:test`.

## Consequences

- #59 (parser and diagnostics) follows the spike's lexer and parser: Rules lexical rules as
  measured, byte offsets on every node, `--` / `++` tokens, path literals, `is`, slices
- #60 (types and built-ins) includes the compiler's static checks (names, functions, methods,
  literal types, string map keys, regex literals). The spike's checker covers what was measured;
  the rest needs a real type checker
- #62 / #89 start from the error findings above: the official emulator's `&&` / `||` is not
  CEL's, non-`resource` errors are 500s, and #6252 (two evaluations) is involved. Which of these
  hidane reproduces is decided there
- `spikes/rules-eval/cases/` and `results/rules-conditions-official-v1.22.0.json` become fixtures
  for the evaluator's tests; the spike code is a reference, not a starting crate
- Dependencies for the evaluator: `regex` for `matches` / `split` / `replace` and a SHA-2 crate
  for `hashing`
- Storage (ADR 0002) must expose a "state after this batch" projection for `getAfter`
- Revisit if cel-rust gains a replaceable grammar and operator layer, or if Rules moves closer
  to CEL
