# Clean-room guide for contributors

hidane's behaviour comes only from the sources in [SOURCES.md](SOURCES.md). This page lists what
must never be used, what to do if you have seen it, and how to say in a pull request where a
behaviour came from.

## Never use

- **Decompiled or disassembled proprietary code**, the official emulator's jar first of all:
  decompiler or disassembler output, `javap -c` listings, class files or resources taken out of
  the jar, and descriptions of its internals written by someone who read them. The same goes for
  any other closed-source Google or Firebase binary.
- **Leaked source code** of the official emulator or of any other Google or Firebase product,
  whatever the place it was found.
- **Confidential Google or Firebase material**: internal documents, design documents, bug
  trackers, chat logs, code or knowledge from working at Google or under a Google NDA, early
  access material shared under confidentiality.
- **Code from implementations under incompatible licenses**: no code is copied, ported or
  translated from projects whose license does not allow it to be distributed under
  `MIT OR Apache-2.0`, such as GPL or AGPL projects, source-available licenses, or code with no
  license at all. Code under a compatible license comes in only as declared third-party code
  (below).
- **Material whose origin you cannot tell** as a source of behaviour: a forum answer, a blog post,
  a snippet or an AI assistant's suggestion may come from any of the above. Find the allowed
  source behind it, or record the behaviour from the official emulator yourself, and cite that.

Code from other open-source Firestore emulators is not copied either; their public README and
documentation helped map the landscape (README, Acknowledgements), which is all hidane takes from
them.

## If you have seen such material

Having read something prohibited does not by itself keep you from contributing, but it has to be
handled openly:

1. **Do not work on the parts it touches.** If you have read the official emulator's decompiled
   code, its leaked source, or confidential material about it, do not write or review hidane code
   for the behaviour it covered.
2. **Tell the maintainers before you contribute in that area.** Open an issue titled "Clean-room
   review" with no details in it, and a maintainer will arrange a private conversation. Say what
   kind of material you saw and which area it covered; never share the material itself.
3. **If it is already in a pull request or in the repository**, say so in the pull request or an
   issue. The maintainers remove it, and the behaviour is recorded again from an allowed source
   by someone who has not seen it.

Anyone who works or has worked at Google on Firebase or Cloud Firestore should talk to the
maintainers before contributing, and check with their employer as for the
[CLA](../cla/README.md#employer-check).

## Citing the source of a behaviour

A pull request that adds or changes behaviour says where the behaviour came from, in its Summary
or its "Parity impact" section:

| Source | Cite |
|---|---|
| The official emulator | The recording and the script that made it: `crates/hidane/tests/fixtures/<name>.json`, recorded with `tools/oracle/<script>` against v1.22.0. A new recording follows [OBSERVATION_POLICY.md](OBSERVATION_POLICY.md) and gets a row in `tools/oracle/README.md` and an entry in `tools/parity/table.py` |
| Public documentation | The page's URL, and the sentence or section it rests on |
| A protobuf definition | The file and the message or field (`google/firestore/v1/write.proto`, `Write.verify`) |
| An SDK or firebase-tools | The repository, version or commit, and the file and function read (`firebase-tools` 15.33.0, `lib/emulator/downloadableEmulators.js`, `_getCommand`); and what was taken from it (a flag, a field, a header) |
| CEL | The section of the specification or the conformance file and case |
| Production Firestore | The documentation page, or the recording made against production (#82) |
| A difference from the official emulator | The row added to [`docs/parity-exceptions.md`](../parity-exceptions.md) |

Code comments name the recording or document a behaviour comes from where it is not obvious
(`tests/fixtures/field_order.json`, `docs/export-format.md`), as hidane's sources already do.

Third-party code (code you did not write) is declared in the pull request with its origin and
license, and kept with its license and notices, as the vendored protos are
([`docs/cla/README.md`](../cla/README.md#what-the-cla-does-not-cover)).

## Checklist

Before opening a pull request that adds or changes behaviour:

- [ ] The behaviour comes from allowed sources ([SOURCES.md](SOURCES.md)), cited in the pull
      request.
- [ ] Nothing in it comes from the official emulator's jar other than its answers: no decompiled,
      disassembled or extracted code, classes or resources.
- [ ] Nothing in it comes from leaked or confidential material, or from code under an
      incompatible license.
- [ ] New recordings were made by a committed script in `tools/oracle/`, against a freshly
      started official emulator under an English locale, with synthetic data and no credentials,
      and hold no paths or details of your machine ([OBSERVATION_POLICY.md](OBSERVATION_POLICY.md)).
- [ ] An intentional difference from the official emulator has its row in
      [`docs/parity-exceptions.md`](../parity-exceptions.md).
- [ ] Third-party code, if any, is declared with its license and notices.
