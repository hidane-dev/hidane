# Security Policy

## Supported versions

hidane is pre-release. Only the latest release on the `main` branch receives security fixes.

## Reporting a vulnerability

Please do **not** open a public issue for security problems.

Use GitHub's private vulnerability reporting:
https://github.com/hidane-dev/hidane/security/advisories/new

You can expect an initial response within 7 days. Once a fix is ready, we will
publish a GitHub Security Advisory and credit the reporter unless you ask otherwise.

## Scope

hidane is a local development emulator. It is not designed to be exposed to
untrusted networks, and reports about running it on a public interface without
authentication are out of scope. Everything else — the gRPC/REST surface, the
Security Rules evaluator, data import/export, and the installer — is in scope.
