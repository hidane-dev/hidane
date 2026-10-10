//! Compiles the vendored googleapis protos (../../proto) with protox (pure Rust, no protoc) and
//! generates prost messages plus tonic servers and clients. The encoded descriptor set is written
//! next to the generated code so the server can expose it through gRPC reflection.

use std::{env, fs, path::PathBuf};

use prost::Message;

const PROTOS: &[&str] = &["google/firestore/v1/firestore.proto"];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("../../proto");
    let out = PathBuf::from(env::var("OUT_DIR")?);
    println!("cargo:rerun-if-changed={}", root.display());

    let fds = protox::Compiler::new([&root])?
        .include_imports(true)
        .include_source_info(true)
        .open_files(PROTOS)?
        .file_descriptor_set();
    fs::write(out.join("hidane_descriptor.bin"), fds.encode_to_vec())?;

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .build_transport(false)
        .generate_default_stubs(true)
        // Firestore orders map keys by UTF-8 bytes, which is exactly `String`'s `Ord`, so a
        // BTreeMap keeps document fields and map values in canonical order for free.
        .btree_map(".google.firestore.v1")
        // Pipeline expressions are never stored. Boxing them keeps `Value` at 32 bytes instead
        // of 72, which matters for every value hidane holds (#27).
        .boxed(".google.firestore.v1.Value.value_type.function_value")
        .boxed(".google.firestore.v1.Value.value_type.pipeline_value")
        .include_file("mod.rs")
        .compile_fds(fds)?;
    Ok(())
}
