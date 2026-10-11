# hidane-rules

Firestore Security Rules for [hidane](https://github.com/hidane-dev/hidane), a Firestore emulator
without Java: the lexer, the parser and the compile-time checks of the official emulator's
`PUT :securityRules`, with its diagnostics.

This crate exists to build the `hidane` binary. Its API follows hidane's needs and may change in
any release; depend on it at your own risk.

Licensed under either of the MIT License or the Apache License, Version 2.0, at your option.
