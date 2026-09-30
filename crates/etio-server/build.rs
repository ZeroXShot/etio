//! Generates the edge/core cluster protocol (`proto/etio/cluster/v1`).

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("../../proto");
    println!("cargo:rerun-if-changed={}", root.join("etio").display());
    let fds = protox::compile(["etio/cluster/v1/cluster.proto"], [&root])?;
    tonic_prost_build::configure().build_server(true).build_client(true).compile_fds(fds)?;
    Ok(())
}
