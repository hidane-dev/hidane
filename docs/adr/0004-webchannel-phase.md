# ADR 0004: Why WebChannel is scheduled for v0.3

- **Status**: Draft (undecided)
- **Date**: 2026-10-09

## Context

In the browser, firebase-js-sdk runs `getDoc` / `getDocs` / `onSnapshot` over the Listen stream and
`setDoc` / `updateDoc` / `deleteDoc` / `writeBatch` over the Write stream, both carried by
WebChannel; `runTransaction` and aggregate queries go over REST (XHR). **A gRPC-only server does
nothing for browser apps.** Flutter Web takes the same path. Server SDKs, mobile SDKs and
firebase-js-sdk in Node work over gRPC alone (see [compatibility.md](../compatibility.md)).

What is known about the WebChannel wire:

| Fact | Source |
|---|---|
| The official emulator bundles a `com.google.net.webchannel.server.v8` server behind `/google.firestore.v1.Firestore/{Listen,Write}/channel`; handshake response `[[0,["c","<SID>","",8,12,30000]]]`; back channel is chunked `text/plain`; `noop` keepalive; `TYPE=terminate` | Observed on v1.22.0 |
| Client wiring (`VER=8`, `RID` / `SID` / `AID` / `gsessionid`, `CI`, `TYPE=xmlhttp`, Authorization carried in the POST body `headers=` field) | closure-library `goog/labs/net/webchannel/*`, js-sdk `webchannel_connection.ts` |
| Unknown: back-channel chunk framing, `CVER`, exact `headers=` encoding, the `[1,5,7]` reply, inactivity timeout | To be captured (issue #87) |
| The official emulator leaks a thread per WebChannel session (firebase-tools #11124, 2026-09) | Issue |
| The only realistic verification is firebase-js-sdk's browser integration suite (Playwright), which upstream itself does not run yet | firebase-js-sdk `packages/firestore/scripts` |

## Options

### A. Ship in v0.3 (current plan)

- WebChannel is an adapter that carries the same bidirectional streams over HTTP/1.1; the core semantics (consistent snapshots, `resume_token`, Write handshake) must be right first
- The unknown framing needs traffic capture and a browser test rig, which is heavy to run alongside v0.1
- gRPC-only v0.1 already has clear users (server, mobile, Node test suites)
- Con: browser developers, the most common local-development case, wait until v0.3; "drop-in" is only partly true before then

### B. Pull ahead of Rules (before v0.2)

- Pro: Web support sooner; Rules could be validated through the Web SDK
- Con: a rules-less emulator used from the browser covers few real workflows

### C. A thin spike in v0.1

- Capture the official emulator's traffic to settle the framing, and guarantee the v0.1 Listen / Write core is callable from a WebChannel adapter
- Pro: removes v0.3's uncertainty early and prevents a transport-coupled core

## Decision

Undecided. Placeholder: **A + C**. Keep WebChannel in v0.3, but in v0.1 (1) pull REST unary
forward so one half of Web support is done, (2) design the Listen / Write core transport-agnostic,
and (3) run the traffic-capture research issue.

## Update (2026-10-10)

The capture (#87) and the server (#71) landed while v0.1 work was still going on: the framing
turned out to be small (see [webchannel.md](../webchannel.md)) and the Listen / Write core was
already transport-agnostic, so WebChannel is an adapter over the same streams. The browser
integration-test runner (#74) and Flutter Web (#75) remain.

## Consequences

- The WebChannel epic (v0.3) and the v0.1 Listen / Write issues reference this ADR
- README and [compatibility.md](../compatibility.md) state "browser: not yet" from v0.1
- The browser integration-test runner is part of v0.3's acceptance criteria
