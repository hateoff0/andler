//! Компилирует `proto/andler.proto` в Rust-код (server+client trait'ы,
//! сообщения) через `tonic-build`/`prost` во время сборки.
//!
//! Требует `protoc` (пакет `protobuf-compiler`) в окружении сборки — см.
//! `docker/Dockerfile.dev`, стадия `builder`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/andler.proto")?;
    Ok(())
}
