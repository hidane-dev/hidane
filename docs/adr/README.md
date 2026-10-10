# Architecture Decision Records

Phase 0 does **not** decide anything. Each ADR below is a draft that lays out the question, the
options and a tentative placeholder. Decisions are made in Phase 1 through the linked issues.

## Format

- **Status**: Draft / Proposed / Accepted / Superseded
- **Context**: why a decision is needed, with primary sources
- **Options**: alternatives with pros, cons and open questions
- **Decision**: "undecided" in Phase 0; a placeholder is marked as such
- **Consequences**: what the decision touches (issues, milestones)

## Index

| # | Topic | Status |
|---|---|---|
| [0001](0001-license.md) | License (MIT placeholder vs MIT OR Apache-2.0) | Draft |
| [0002](0002-storage-engine.md) | Storage engine (in-memory / redb / SQLite) | Accepted |
| [0003](0003-grpc-stack.md) | gRPC stack (tonic) and sharing one port with REST / WebChannel | Accepted |
| [0004](0004-webchannel-phase.md) | Why WebChannel is scheduled for v0.3 | Draft |
| [0005](0005-rules-engine.md) | Security Rules evaluator approach | Draft |
