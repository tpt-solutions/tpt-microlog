//! Compiles the OpenTelemetry protos used by the gRPC transport.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protos = [
        "proto/opentelemetry/proto/common/v1/common.proto",
        "proto/opentelemetry/proto/resource/v1/resource.proto",
        "proto/opentelemetry/proto/logs/v1/logs.proto",
    ];
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&protos, &["proto/"])?;
    println!("cargo:rerun-if-changed=proto/");
    Ok(())
}
