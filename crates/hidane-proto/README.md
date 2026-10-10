# hidane-proto

Generated protobuf messages and gRPC services for `google.firestore.v1` and the googleapis
packages it depends on, built for [hidane](https://github.com/hidane-dev/hidane), a Firestore
emulator without Java. The `.proto` files are vendored in `proto/` (Apache License 2.0, see
`proto/README.md`) and compiled with protox, so no `protoc` is needed.

This crate exists to build the `hidane` binary. Its API follows hidane's needs and may change in
any release; depend on it at your own risk.

Licensed under either of the MIT License or the Apache License, Version 2.0, at your option.
