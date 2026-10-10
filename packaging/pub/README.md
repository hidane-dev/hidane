# hidane

**hidane** (火種, *the seed of fire*) — a Firestore emulator without Java. A single binary that
speaks the official emulator's gRPC, REST and WebChannel protocols, starts in milliseconds and is
tested against the official emulator. Not affiliated with Google.

This package is a launcher: the first run downloads hidane's release binary for your platform
(macOS arm64 / x64, Linux arm64 / x64, Windows x64) from GitHub Releases, checks its SHA-256 and
keeps it in a cache directory.

```sh
dart pub global activate hidane

# Under firebase-tools, in place of the official emulator (no Java needed):
hidane exec -- firebase emulators:start --only firestore

# Or on its own:
hidane --host 127.0.0.1 --port 8080
export FIRESTORE_EMULATOR_HOST=127.0.0.1:8080
```

`HIDANE_CACHE_DIR` moves the cache; `HIDANE_RELEASES_URL` points at a mirror of the release files
(`v<version>/<file>`).

What works today, what does not yet (Security Rules), and how parity with the official emulator is
checked: https://github.com/hidane-dev/hidane

Licensed under either of the MIT License or the Apache License, Version 2.0, at your option.
