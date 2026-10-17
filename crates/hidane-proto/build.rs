//! Compiles the vendored googleapis protos (`proto/`) with protox (pure Rust, no protoc) and
//! generates prost messages plus tonic servers and clients. The encoded descriptor set is written
//! next to the generated code so the server can expose it through gRPC reflection.
//!
//! `Document`, `MapValue` and `AggregationResult` are not generated: `src/ordered.rs` has them,
//! with maps that keep their order (#108). A second descriptor set describes their map fields
//! as repeated entries, so JSON handled through it keeps the order too.

use std::{env, fs, path::PathBuf};

use prost::Message;

const PROTOS: &[&str] = &["google/firestore/v1/firestore.proto"];

/// Datastore mode's API (#81), generated but not in the descriptor sets above: hidane does not
/// serve it yet, and reflection lists what the descriptors hold.
const DATASTORE: &[&str] = &["google/datastore/v1/datastore.proto"];

/// The messages written by hand in `src/ordered.rs`: package, message, the Rust type, and the map
/// entry it holds.
const ORDERED: &[(&str, &str, &str, &str)] = &[
    (
        "google.firestore.v1",
        "Document",
        "crate::ordered::Document",
        "FieldsEntry",
    ),
    (
        "google.firestore.v1",
        "MapValue",
        "crate::ordered::MapValue",
        "FieldsEntry",
    ),
    (
        "google.firestore.v1",
        "AggregationResult",
        "crate::ordered::AggregationResult",
        "AggregateFieldsEntry",
    ),
    (
        "google.datastore.v1",
        "Entity",
        "crate::ordered::Entity",
        "PropertiesEntry",
    ),
    (
        "google.datastore.v1",
        "AggregationResult",
        "crate::ordered::DatastoreAggregationResult",
        "AggregatePropertiesEntry",
    ),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("proto");
    let out = PathBuf::from(env::var("OUT_DIR")?);
    println!("cargo:rerun-if-changed={}", root.display());

    let fds = protox::Compiler::new([&root])?
        .include_imports(true)
        .include_source_info(true)
        .open_files(PROTOS)?
        .file_descriptor_set();
    fs::write(out.join("hidane_descriptor.bin"), fds.encode_to_vec())?;

    // The code of both APIs, from one descriptor set. JSON reads both, through the ordered
    // descriptors; reflection lists Firestore's alone (above).
    let all = protox::Compiler::new([&root])?
        .include_imports(true)
        .include_source_info(true)
        .open_files(PROTOS.iter().chain(DATASTORE))?
        .file_descriptor_set();
    fs::write(
        out.join("hidane_descriptor_ordered.bin"),
        ordered(all.clone()).encode_to_vec(),
    )?;

    let mut builder = tonic_prost_build::configure();
    for (package, message, rust, _) in ORDERED {
        builder = builder.extern_path(format!(".{package}.{message}"), *rust);
    }
    builder
        .build_server(true)
        .build_client(true)
        .build_transport(false)
        .generate_default_stubs(true)
        // Firestore orders map keys by UTF-8 bytes, which is exactly `String`'s `Ord`, so a
        // BTreeMap keeps document fields and map values in canonical order for free.
        .btree_map(".google.firestore.v1")
        .btree_map(".google.datastore.v1")
        // Pipeline expressions are never stored. Boxing them keeps `Value` at 32 bytes instead
        // of 72, which matters for every value hidane holds (#27).
        .boxed(".google.firestore.v1.Value.value_type.function_value")
        .boxed(".google.firestore.v1.Value.value_type.pipeline_value")
        .include_file("mod.rs")
        .compile_fds(all)?;

    // The hand-written messages, where the generated ones would have been.
    let v1 = out.join("google.firestore.v1.rs");
    let mut generated = fs::read_to_string(&v1)?;
    generated.push_str("\npub use crate::ordered::{AggregationResult, Document, MapValue};\n");
    fs::write(v1, generated)?;
    let datastore = out.join("google.datastore.v1.rs");
    let mut generated = fs::read_to_string(&datastore)?;
    generated.push_str(
        "\npub use crate::ordered::{DatastoreAggregationResult as AggregationResult, Entity};\n",
    );
    fs::write(datastore, generated)?;
    Ok(())
}

/// The descriptors with the ordered messages' maps turned into repeated entries.
fn ordered(mut fds: prost_types::FileDescriptorSet) -> prost_types::FileDescriptorSet {
    for file in &mut fds.file {
        let package = file.package().to_owned();
        for message in &mut file.message_type {
            let Some((_, _, _, entry)) = ORDERED
                .iter()
                .find(|(p, m, _, _)| *p == package && *m == message.name())
            else {
                continue;
            };
            for nested in &mut message.nested_type {
                if nested.name() == *entry
                    && let Some(options) = &mut nested.options
                {
                    options.map_entry = None;
                }
            }
        }
    }
    fds
}
