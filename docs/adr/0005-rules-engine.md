# ADR 0005: Security Rules evaluator approach

- **Status**: Draft (undecided)
- **Date**: 2026-10-09

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

Oracles: the official emulator (differential tests) and the production Rules API
`projects:test`, which returns diagnostics and evaluation results without a database.
Known official-emulator bug #6252 is not reproduced (see [parity-exceptions.md](../parity-exceptions.md)).

## Options

### A. Hand-written parser + tree-walking interpreter (Rust)

- Pro: full control over Rules-specific types, methods, error semantics and diagnostics (`issues[]` with positions)
- Con: 70+ methods and conversions to write; CEL subtleties tracked by hand

### B. Reuse cel-rust (`cel-interpreter`) for expressions, extend with Rules syntax and types

- Pro: CEL evaluation, types and error semantics for free
- Con: Rules types (Path, Set, MapDiff, Timestamp methods) and `is` / `in` semantics may not match; a fork if the extension points are insufficient
- Open: cel-rust maintenance status and extension API

### C. A for grammar and types, B tried for expression evaluation only (spike)

### `list` implication (common to all options)

- v0.2 ships a conservative subset (`==` `!=` `<` `<=` `>` `>=` `in` `or` `array-contains`
  `array-contains-any` combined with `resource.data.<field>` / `resource.id` / `request.auth.*`).
  Undecidable expressions are **denied** with a logged reason
- The subset is widened by measuring the official emulator

## Decision

Undecided. Placeholder: **A** for grammar, types and diagnostics, with the **C spike** at the start
of v0.2 to decide whether B replaces the expression evaluator. Golden results come from both the
production Rules API and the official emulator.

## Consequences

- The first v0.2 issue is the parser and diagnostics format; the evaluator approach is fixed after the spike
- Storage (ADR 0002) must expose a "state after this batch" projection for `getAfter`
- Error semantics are measured on the official emulator before implementation
