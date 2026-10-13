//! What hidane has verified, and its known gaps, with where each is recorded. A finding is
//! `compatible` only through an entry here; keep the entries in step with
//! `docs/compatibility.md`, `results/README.md` and `docs/parity-exceptions.md`.

/// A client or tool verified against hidane: its results were compared with the official
/// emulator's.
pub struct Verified {
    pub ecosystem: &'static str,
    pub name: &'static str,
    /// The version that was verified. Another version with the same major version counts as
    /// verified; any other does not.
    pub version: &'static str,
    /// What was verified with it.
    pub what: &'static str,
    pub source: &'static str,
}

pub const VERIFIED: &[Verified] = &[
    Verified {
        ecosystem: "npm",
        name: "firebase",
        version: "13.0.0",
        what: "in Node over gRPC, Lite over REST, and in Chromium over WebChannel",
        source: "docs/compatibility.md; results/sdk-web-writes-*.json, results/sdk-rest-*.json, results/browser-webchannel-*.json",
    },
    Verified {
        ecosystem: "npm",
        name: "@google-cloud/firestore",
        version: "9.3.1",
        what: "documents, queries, aggregations, transactions, listeners",
        source: "docs/compatibility.md; results/sdk-documents-*.json, results/sdk-queries-*.json, results/sdk-listen-*.json",
    },
    Verified {
        ecosystem: "npm",
        name: "@firebase/rules-unit-testing",
        version: "6.0.0",
        what: "assertSucceeds / assertFails under Security Rules, clearFirestore()",
        source: "docs/compatibility.md; results/sdk-rules-unit-testing-*.json, results/browser-rules-*.json",
    },
    Verified {
        ecosystem: "npm",
        name: "firebase-tools",
        version: "15.33.0",
        what: "emulators:start and emulators:exec through hidane exec, without Java",
        source: "docs/adr/0006-launch-under-firebase-tools.md; results/export-import-*.json",
    },
    Verified {
        ecosystem: "npm",
        name: "firebase-functions",
        version: "6.6.0",
        what: "v1 and v2 Firestore triggers, withAuthContext too, under the Functions emulator",
        source: "docs/compatibility.md; results/functions-triggers-*.json",
    },
    Verified {
        ecosystem: "pypi",
        name: "google-cloud-firestore",
        version: "2.34.1",
        what: "documents, queries, aggregations, transactions, listeners",
        source: "docs/compatibility.md; results/sdk-python-*.json",
    },
    Verified {
        ecosystem: "go",
        name: "cloud.google.com/go/firestore",
        version: "1.26.0",
        what: "documents, queries, aggregations, transactions, listeners",
        source: "docs/compatibility.md; results/sdk-go-*.json",
    },
    Verified {
        ecosystem: "maven",
        name: "com.google.cloud:google-cloud-firestore",
        version: "3.49.0",
        what: "documents, queries, aggregations, transactions, listeners",
        source: "docs/compatibility.md; results/sdk-java-*.json",
    },
    Verified {
        ecosystem: "pub",
        name: "cloud_firestore",
        version: "6.10.0",
        what: "Flutter Web in Chromium",
        source: "docs/compatibility.md; results/flutter-web-*.json",
    },
];

pub fn verified(ecosystem: &str, name: &str) -> Option<&'static Verified> {
    VERIFIED
        .iter()
        .find(|v| v.ecosystem == ecosystem && v.name == name)
}

impl Verified {
    pub fn major(&self) -> u64 {
        super::deps::major(self.version).unwrap_or(0)
    }
}

/// The Emulator UI version verified with hidane.
pub const EMULATOR_UI: (&str, &str) = (
    "Emulator UI v1.15.0: browse, create and delete documents, Clear all data, the request monitor",
    "docs/compatibility.md",
);

/// Imports and exports in the official format.
pub const EXPORT_IMPORT: (&str, &str) = (
    "--import and --export-on-exit read and write the official emulator's format",
    "docs/export-format.md; results/export-import-*.json",
);

/// Security Rules compiled with the official compiler's checks and messages.
pub const RULES_COMPILER: &str =
    "crates/hidane/tests/fixtures/rules_compile.json; docs/parity-exceptions.md";

/// Composite indexes: neither emulator enforces them.
pub const INDEXES: &str = "docs/parity-exceptions.md (Composite indexes)";

/// Known gaps.
pub const GAP_PIPELINES: &str =
    "https://github.com/hidane-dev/hidane/issues/80; docs/parity-exceptions.md (ExecutePipeline)";
pub const GAP_DATASTORE: &str =
    "https://github.com/hidane-dev/hidane/issues/81; docs/parity-exceptions.md";
pub const WINDOWS: &str = "docs/adr/0006-launch-under-firebase-tools.md (Consequences)";
