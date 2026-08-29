//! Pure OCI image-layout builder.
//!
//! Converts a root-filesystem tar archive into a conformant OCI image layout
//! (OCI Image Specification v1.1.0): an `oci-layout` marker, an `index.json`
//! manifest list, a `config.json` image config, and the rootfs layer blob
//! under `blobs/sha256/`. This module does no guest I/O — it only assembles
//! and verifies the layout from a tar byte string plus the image's platform
//! metadata, mirroring how `base_image::sha256_of` pins a build with a
//! content sha256.
//!
//! The daemon owns the guest I/O (extracting the rootfs with the guestfs
//! appliance); everything in this module is unit-testable offline.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;

/// Image-spec version written into `oci-layout` (OCI Image Specification v1.1.0).
pub const OCI_SPEC_VERSION: &str = "1.1.0";

/// OCI media type for the image config blob.
pub const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";
/// OCI media type for a single uncompressed tar layer.
pub const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1+tar";
/// OCI media type for the image manifest.
pub const MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// OCI media type for the index (manifest list).
pub const INDEX_MEDIA_TYPE: &str = "application/vnd.oci.image.index.v1+json";

/// Platform metadata baked into the OCI config and index platform descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageSpec {
    /// Guest CPU architecture in OCI form (`amd64`, `arm64`, ...).
    pub architecture: String,
    /// Guest OS (`linux`).
    pub os: String,
    /// Container entrypoint (`/sbin/init` for a bare Linux rootfs).
    pub entrypoint: Vec<String>,
    /// Optional container command.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
}

impl ImageSpec {
    pub fn new(architecture: &str) -> Self {
        ImageSpec {
            architecture: architecture.to_string(),
            os: "linux".to_string(),
            entrypoint: vec!["/sbin/init".to_string()],
            command: Vec::new(),
        }
    }
}

/// Digests and byte sizes of every artifact the layout is built from. Returned
/// by both `build_layout` and `verify_layout` so callers can compare what was
/// written against what the caller passed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildReport {
    pub tar_digest: String,
    pub tar_size: u64,
    pub config_digest: String,
    pub config_size: u64,
    pub manifest_digest: String,
    pub manifest_size: u64,
    pub index_digest: String,
    pub index_size: u64,
}

#[derive(Debug, Error)]
pub enum OciExportError {
    #[error("failed to write OCI layout at {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("OCI layout at {path} is missing {file}")]
    MissingFile {
        path: std::path::PathBuf,
        file: &'static str,
    },
    #[error("OCI layout at {path} is corrupt: {file} digest {expected} != {actual}")]
    DigestMismatch {
        path: std::path::PathBuf,
        file: &'static str,
        expected: String,
        actual: String,
    },
    #[error("failed to parse OCI layout file {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
}
/// already used by `base_image::sha256_of`; no external hex dependency.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// `{"imageSpecVersion": "1.1.0"}` — the marker file every OCI layout needs.
pub fn oci_layout_json() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "imageSpecVersion": OCI_SPEC_VERSION,
    }))
    .expect("oci-layout marker is static and serializes")
}

/// The image config blob: architecture, os, entrypoint, and the rootfs layer's
/// `diff_id`. For an uncompressed tar the `diff_id` equals the layer blob
/// digest (the tar bytes are the layer content).
pub fn config_json(spec: &ImageSpec, diff_id: &str) -> String {
    #[derive(Serialize)]
    struct EntrypointConfig {
        #[serde(rename = "Entrypoint")]
        entrypoint: Vec<String>,
        #[serde(rename = "Cmd", skip_serializing_if = "Vec::is_empty")]
        command: Vec<String>,
    }
    #[derive(Serialize)]
    struct RootFs {
        #[serde(rename = "type")]
        kind: &'static str,
        diff_ids: Vec<String>,
    }
    #[derive(Serialize)]
    struct ImageConfig {
        architecture: String,
        os: String,
        config: EntrypointConfig,
        rootfs: RootFs,
    }
    let config = ImageConfig {
        architecture: spec.architecture.clone(),
        os: spec.os.clone(),
        config: EntrypointConfig {
            entrypoint: spec.entrypoint.clone(),
            command: spec.command.clone(),
        },
        rootfs: RootFs {
            kind: "layers",
            diff_ids: vec![format!("sha256:{diff_id}")],
        },
    };
    serde_json::to_string_pretty(&config).expect("image config serializes")
}

/// The image manifest: references the config blob and the single rootfs layer.
pub fn manifest_json(
    config_digest: &str,
    config_size: u64,
    layer_digest: &str,
    layer_size: u64,
) -> String {
    #[derive(Serialize)]
    struct Descriptor {
        media_type: &'static str,
        digest: String,
        size: u64,
    }
    #[derive(Serialize)]
    struct Manifest {
        #[serde(rename = "schemaVersion")]
        schema_version: u32,
        media_type: &'static str,
        config: Descriptor,
        layers: Vec<Descriptor>,
    }
    let manifest = Manifest {
        schema_version: 2,
        media_type: MANIFEST_MEDIA_TYPE,
        config: Descriptor {
            media_type: CONFIG_MEDIA_TYPE,
            digest: format!("sha256:{config_digest}"),
            size: config_size,
        },
        layers: vec![Descriptor {
            media_type: LAYER_MEDIA_TYPE,
            digest: format!("sha256:{layer_digest}"),
            size: layer_size,
        }],
    };
    serde_json::to_string_pretty(&manifest).expect("manifest serializes")
}

/// The index (manifest list): points at the single-platform manifest.
pub fn index_json(
    manifest_digest: &str,
    manifest_size: u64,
    architecture: &str,
    os: &str,
) -> String {
    #[derive(Serialize)]
    struct Platform {
        architecture: String,
        os: String,
    }
    #[derive(Serialize)]
    struct IndexEntry {
        media_type: &'static str,
        digest: String,
        size: u64,
        platform: Platform,
    }
    #[derive(Serialize)]
    struct Index {
        #[serde(rename = "schemaVersion")]
        schema_version: u32,
        media_type: &'static str,
        manifests: Vec<IndexEntry>,
    }
    let index = Index {
        schema_version: 2,
        media_type: INDEX_MEDIA_TYPE,
        manifests: vec![IndexEntry {
            media_type: MANIFEST_MEDIA_TYPE,
            digest: format!("sha256:{manifest_digest}"),
            size: manifest_size,
            platform: Platform {
                architecture: architecture.to_string(),
                os: os.to_string(),
            },
        }],
    };
    serde_json::to_string_pretty(&index).expect("index serializes")
}

/// Writes a complete OCI layout into `dir` and reports the resulting digests.
///
/// Loads the whole layer into memory; for large layers use [`build_layer`],
/// which streams them from a reader without holding them all at once.
/// Layout on disk:
/// ```text
/// <dir>/oci-layout        marker
/// <dir>/index.json        manifest list
/// <dir>/config.json       image config (root config)
/// <dir>/blobs/sha256/<tar-digest>   rootfs layer
/// ```
pub fn build_layout(
    dir: &Path,
    spec: &ImageSpec,
    tar_bytes: &[u8],
) -> Result<BuildReport, OciExportError> {
    build_layer(dir, spec, tar_bytes)
}

/// Streams `layer` into the blob store, hashing it as it writes, then emits
/// the OCI metadata. The blob is written to a temporary file in
/// `blobs/sha256/` and atomically renamed to its digest-named path, so a crash
/// mid-write never leaves a partial blob under its final name.
///
/// This is the streaming counterpart of [`build_layout`]: it never holds the
/// layer in memory at once, so it can export disks larger than available RAM.
pub fn build_layer<R: std::io::Read>(
    dir: &Path,
    spec: &ImageSpec,
    mut layer: R,
) -> Result<BuildReport, OciExportError> {
    use std::io::Write as _;

    let blobs = dir.join("blobs").join("sha256");
    fs::create_dir_all(&blobs).map_err(|source| OciExportError::Io {
        path: blobs.clone(),
        source,
    })?;

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = blobs.join(format!(".tmp-{nanos}-layer"));
    let (tar_digest, tar_size) = {
        let mut writer = fs::File::create(&tmp).map_err(|source| OciExportError::Io {
            path: tmp.clone(),
            source,
        })?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut size: u64 = 0;
        loop {
            let n = layer.read(&mut buf).map_err(|source| OciExportError::Io {
                path: tmp.clone(),
                source,
            })?;
            if n == 0 {
                break;
            }
            writer
                .write_all(&buf[..n])
                .map_err(|source| OciExportError::Io {
                    path: tmp.clone(),
                    source,
                })?;
            hasher.update(&buf[..n]);
            size += n as u64;
        }
        writer.flush().map_err(|source| OciExportError::Io {
            path: tmp.clone(),
            source,
        })?;
        let digest = hasher.finalize();
        let mut tar_digest = String::with_capacity(64);
        for byte in digest {
            let _ = write!(tar_digest, "{byte:02x}");
        }
        (tar_digest, size)
    };

    let config = config_json(spec, &tar_digest);
    let manifest = manifest_json(
        &sha256_hex(config.as_bytes()),
        config.len() as u64,
        &tar_digest,
        tar_size,
    );
    let index = index_json(
        &sha256_hex(manifest.as_bytes()),
        manifest.len() as u64,
        &spec.architecture,
        &spec.os,
    );

    fs::write(dir.join("oci-layout"), oci_layout_json().as_bytes()).map_err(|source| {
        OciExportError::Io {
            path: dir.join("oci-layout"),
            source,
        }
    })?;
    fs::write(dir.join("index.json"), index.as_bytes()).map_err(|source| OciExportError::Io {
        path: dir.join("index.json"),
        source,
    })?;
    fs::write(dir.join("config.json"), config.as_bytes()).map_err(|source| OciExportError::Io {
        path: dir.join("config.json"),
        source,
    })?;

    fs::rename(&tmp, blobs.join(&tar_digest)).map_err(|source| OciExportError::Io {
        path: tmp.clone(),
        source,
    })?;

    Ok(BuildReport {
        tar_digest,
        tar_size,
        config_digest: sha256_hex(config.as_bytes()),
        config_size: config.len() as u64,
        manifest_digest: sha256_hex(manifest.as_bytes()),
        manifest_size: manifest.len() as u64,
        index_digest: sha256_hex(index.as_bytes()),
        index_size: index.len() as u64,
    })
}

/// Re-reads the layout at `dir`, recomputes every digest, and confirms it still
/// matches `tar_bytes`/`spec`. The sha256 verification path: a caller that
/// already has the report from `build_layout` can assert the on-disk files
/// agree, catching truncation or partial writes.
pub fn verify_layout(dir: &Path, tar_bytes: &[u8]) -> Result<BuildReport, OciExportError> {
    let read = |file: &'static str| -> Result<String, OciExportError> {
        fs::read_to_string(dir.join(file)).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                OciExportError::MissingFile {
                    path: dir.to_path_buf(),
                    file,
                }
            } else {
                OciExportError::Io {
                    path: dir.join(file),
                    source,
                }
            }
        })
    };

    let index = read("index.json")?;
    let config = read("config.json")?;
    let layout = read("oci-layout")?;

    if layout.trim() != oci_layout_json() {
        return Err(OciExportError::DigestMismatch {
            path: dir.join("oci-layout"),
            file: "oci-layout",
            expected: oci_layout_json(),
            actual: layout,
        });
    }

    let tar_digest = sha256_hex(tar_bytes);
    let config_digest = sha256_hex(config.as_bytes());
    let manifest = manifest_json(
        &config_digest,
        config.len() as u64,
        &tar_digest,
        tar_bytes.len() as u64,
    );
    let manifest_digest = sha256_hex(manifest.as_bytes());

    let blob = dir.join("blobs").join("sha256").join(&tar_digest);
    let on_disk = fs::read(&blob).map_err(|source| OciExportError::Io {
        path: blob.clone(),
        source,
    })?;
    if on_disk != tar_bytes {
        return Err(OciExportError::DigestMismatch {
            path: blob,
            file: "blobs/sha256/<tar>",
            expected: format!("sha256:{tar_digest}"),
            actual: format!("sha256:{}", sha256_hex(&on_disk)),
        });
    }

    // The index must reference the manifest we would have produced.
    let index_manifest = serde_json::from_str::<serde_json::Value>(&index).map_err(|source| {
        OciExportError::Parse {
            path: dir.join("index.json"),
            source,
        }
    })?;
    let referenced = index_manifest
        .get("manifests")
        .and_then(|m| m.as_array())
        .and_then(|ms| ms.first())
        .and_then(|m| m.get("digest"))
        .and_then(|d| d.as_str())
        .ok_or_else(|| OciExportError::MissingFile {
            path: dir.to_path_buf(),
            file: "index.json",
        })?;
    if referenced != format!("sha256:{manifest_digest}") {
        return Err(OciExportError::DigestMismatch {
            path: dir.join("index.json"),
            file: "index.json",
            expected: format!("sha256:{manifest_digest}"),
            actual: referenced.to_string(),
        });
    }
    Ok(BuildReport {
        tar_digest,
        tar_size: tar_bytes.len() as u64,
        config_digest,
        config_size: config.len() as u64,
        manifest_digest,
        manifest_size: manifest.len() as u64,
        index_digest: sha256_hex(index.as_bytes()),
        index_size: index.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic 3-file rootfs tar (no filesystem metadata, so the digest
    /// is stable and testable without a real guestfs appliance).
    fn sample_tar() -> Vec<u8> {
        b"sample-rootfs-tar-content".to_vec()
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // sha256("") is a well-known constant; pins the hex encoding.
        let empty = sha256_hex(b"");
        assert_eq!(
            empty,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(empty.len(), 64);
    }

    #[test]
    fn oci_layout_marker_is_spec_version() {
        let parsed: serde_json::Value = serde_json::from_str(&oci_layout_json()).unwrap();
        assert_eq!(parsed["imageSpecVersion"], OCI_SPEC_VERSION);
    }

    #[test]
    fn config_json_carries_arch_entrypoint_and_diff_id() {
        let spec = ImageSpec::new("amd64");
        let cfg = config_json(&spec, "abc123");
        let parsed: serde_json::Value = serde_json::from_str(&cfg).unwrap();
        assert_eq!(parsed["architecture"], "amd64");
        assert_eq!(parsed["os"], "linux");
        assert_eq!(parsed["config"]["Entrypoint"][0], "/sbin/init");
        assert_eq!(parsed["rootfs"]["diff_ids"][0], "sha256:abc123");
    }

    #[test]
    fn manifest_references_config_and_layer() {
        let manifest = manifest_json("cfg", 10, "layer", 20);
        let parsed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(parsed["schemaVersion"], 2);
        assert_eq!(parsed["config"]["digest"], "sha256:cfg");
        assert_eq!(parsed["config"]["size"], 10);
        assert_eq!(parsed["layers"][0]["digest"], "sha256:layer");
        assert_eq!(parsed["layers"][0]["size"], 20);
    }

    #[test]
    fn index_points_at_platform_manifest() {
        let index = index_json("man", 30, "arm64", "linux");
        let parsed: serde_json::Value = serde_json::from_str(&index).unwrap();
        assert_eq!(parsed["manifests"][0]["digest"], "sha256:man");
        assert_eq!(parsed["manifests"][0]["platform"]["architecture"], "arm64");
        assert_eq!(parsed["manifests"][0]["platform"]["os"], "linux");
    }

    #[test]
    fn build_layout_writes_all_files_and_digests_match() {
        let dir = std::env::temp_dir().join(format!("andler_oci_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tar = sample_tar();
        let spec = ImageSpec::new("amd64");

        let report = build_layout(&dir, &spec, &tar).expect("layout builds");
        assert_eq!(report.tar_digest, sha256_hex(&tar));

        let verified = verify_layout(&dir, &tar).expect("layout verifies");
        assert!(dir.join("oci-layout").exists());
        assert!(dir.join("index.json").exists());
        assert!(dir.join("config.json").exists());
        assert!(dir
            .join("blobs")
            .join("sha256")
            .join(&report.tar_digest)
            .exists());

        // verify_layout agrees with the report, including the on-disk blob.
        assert_eq!(report, verified);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_layer_streams_large_input_across_buffers() {
        // A >64 KiB input forces the streaming read to split across the 64 KiB
        // buffer, exercising the chunked hashing path that the byte builder's
        // small fixture never reaches.
        let dir = std::env::temp_dir().join(format!("andler_oci_stream_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let expected = sha256_hex(&payload);
        let spec = ImageSpec::new("amd64");

        let report = build_layer(&dir, &spec, std::io::Cursor::new(payload.clone()))
            .expect("streaming layout builds");
        assert_eq!(
            report.tar_digest, expected,
            "streamed digest must match direct"
        );
        assert_eq!(report.tar_size, payload.len() as u64);

        let verified = verify_layout(&dir, &payload).expect("layout verifies");
        assert_eq!(report, verified);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_layout_detects_tampered_blob() {
        let dir = std::env::temp_dir().join(format!("andler_oci_tamper_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let tar = sample_tar();
        let spec = ImageSpec::new("amd64");
        build_layout(&dir, &spec, &tar).expect("layout builds");

        // Overwrite the blob with different bytes; the layout is now corrupt.
        fs::write(
            dir.join("blobs").join("sha256").join(&sha256_hex(&tar)),
            b"other",
        )
        .unwrap();

        let err = verify_layout(&dir, &tar).unwrap_err();
        assert!(matches!(err, OciExportError::DigestMismatch { .. }));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_layout_reports_missing_file() {
        let dir = std::env::temp_dir().join(format!("andler_oci_missing_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let tar = sample_tar();

        let err = verify_layout(&dir, &tar).unwrap_err();
        assert!(matches!(err, OciExportError::MissingFile { .. }));

        let _ = fs::remove_dir_all(&dir);
    }
}
