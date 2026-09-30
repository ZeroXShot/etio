//! Generates the OTLP message types, gRPC clients, and raw-bytes gRPC servers
//! from the vendored protocol definitions (`proto/`).
//!
//! Protos are parsed with `protox`, a pure-Rust compiler, so building Etio
//! needs no system `protoc`.

use std::path::PathBuf;

const PROTOS: &[&str] = &[
    "opentelemetry/proto/collector/trace/v1/trace_service.proto",
    "opentelemetry/proto/collector/metrics/v1/metrics_service.proto",
    "opentelemetry/proto/collector/logs/v1/logs_service.proto",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("../../proto");
    println!("cargo:rerun-if-changed={}", root.display());

    let fds = protox::compile(PROTOS, [&root])?;

    // Message types and clients (used by the simulator, the load generator
    // and the differential tests of the fast decoders).
    tonic_prost_build::configure().build_server(false).build_client(true).compile_fds(fds)?;

    // Servers that hand over the raw request bytes, so that requests can be
    // decoded selectively instead of being materialised as message trees.
    let raw = |service: &str, package: &str, response: &str| {
        tonic_build::manual::Service::builder()
            .name(service)
            .package(package)
            .method(
                tonic_build::manual::Method::builder()
                    .name("export")
                    .route_name("Export")
                    .input_type("::bytes::Bytes")
                    .output_type(response)
                    .codec_path("crate::grpc::RawCodec")
                    .build(),
            )
            .build()
    };
    // Same packages as OTLP (they define the gRPC routes); a separate output
    // directory keeps these files from overwriting the message modules.
    let raw_dir = PathBuf::from(std::env::var("OUT_DIR")?).join("raw");
    std::fs::create_dir_all(&raw_dir)?;
    tonic_build::manual::Builder::new().build_client(false).out_dir(&raw_dir).compile(&[
        raw(
            "TraceService",
            "opentelemetry.proto.collector.trace.v1",
            "crate::proto::collector::trace::v1::ExportTraceServiceResponse",
        ),
        raw(
            "MetricsService",
            "opentelemetry.proto.collector.metrics.v1",
            "crate::proto::collector::metrics::v1::ExportMetricsServiceResponse",
        ),
        raw(
            "LogsService",
            "opentelemetry.proto.collector.logs.v1",
            "crate::proto::collector::logs::v1::ExportLogsServiceResponse",
        ),
    ]);
    Ok(())
}
