use serde::{Deserialize, Serialize};

use crate::MutatorOp;

/// Guest provisioning manifest (§7): a declarative list of file mutations
/// applied through a `GuestMutator` — online via QGA when the instance is
/// running, offline through the guestfs appliance when stopped. One
/// manifest = one `guest provision <file> <id>` call; manifests live in
/// `docker/images/guest-components/<component>/manifest.toml`.
///
/// `host_path` on `upload-file` is resolved by the CLI against the
/// manifest's own directory before the request leaves the client; the
/// daemon only ever sees absolute paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionManifest {
    pub schema_version: u32,
    #[serde(default)]
    pub name: String,
    pub ops: Vec<ManifestOp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ManifestOp {
    WriteFile {
        path: String,
        content: String,
        /// Octal string like "0644"; expanded into a separate chmod op.
        #[serde(default)]
        mode: Option<String>,
    },
    UploadFile {
        guest_path: String,
        host_path: String,
        #[serde(default)]
        mode: Option<String>,
    },
    MkdirP {
        path: String,
    },
    CpA {
        src: String,
        dst: String,
    },
    Mv {
        src: String,
        dst: String,
    },
    RmRf {
        path: String,
    },
    Chmod {
        path: String,
        mode: String,
    },
    Symlink {
        target: String,
        link: String,
    },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProvisionError {
    #[error("provision manifest: {0}")]
    Parse(String),
    #[error("provision manifest: op {index} ({op}): {message}")]
    Op {
        index: usize,
        op: &'static str,
        message: String,
    },
}

impl ProvisionManifest {
    pub fn parse(toml_input: &str) -> Result<Self, ProvisionError> {
        let manifest: ProvisionManifest =
            toml::from_str(toml_input).map_err(|e| ProvisionError::Parse(e.to_string()))?;
        if manifest.schema_version != 1 {
            return Err(ProvisionError::Parse(format!(
                "unsupported schema_version {} (expected 1)",
                manifest.schema_version
            )));
        }
        Ok(manifest)
    }

    /// Expands the manifest into the flat `MutatorOp` list the daemon
    /// applies. `mode` on write/upload becomes a trailing chmod op so the
    /// wire format stays a 1:1 mirror of `MutatorOp`.
    pub fn to_mutator_ops(&self) -> Result<Vec<MutatorOp>, ProvisionError> {
        let mut out = Vec::new();
        for (index, op) in self.ops.iter().enumerate() {
            match op {
                ManifestOp::WriteFile {
                    path,
                    content,
                    mode,
                } => {
                    out.push(MutatorOp::WriteFile {
                        path: path.clone(),
                        content: content.as_bytes().to_vec(),
                    });
                    if let Some(mode) = mode {
                        out.push(MutatorOp::Chmod {
                            path: path.clone(),
                            mode: parse_mode(index, "write-file", mode)?,
                        });
                    }
                }
                ManifestOp::UploadFile {
                    guest_path,
                    host_path,
                    mode,
                } => {
                    out.push(MutatorOp::UploadFile {
                        path: guest_path.clone(),
                        host_path: std::path::PathBuf::from(host_path),
                    });
                    if let Some(mode) = mode {
                        out.push(MutatorOp::Chmod {
                            path: guest_path.clone(),
                            mode: parse_mode(index, "upload-file", mode)?,
                        });
                    }
                }
                ManifestOp::MkdirP { path } => out.push(MutatorOp::MkdirP { path: path.clone() }),
                ManifestOp::CpA { src, dst } => out.push(MutatorOp::CpA {
                    src: src.clone(),
                    dst: dst.clone(),
                }),
                ManifestOp::Mv { src, dst } => out.push(MutatorOp::Mv {
                    src: src.clone(),
                    dst: dst.clone(),
                }),
                ManifestOp::RmRf { path } => out.push(MutatorOp::RmRf { path: path.clone() }),
                ManifestOp::Chmod { path, mode } => out.push(MutatorOp::Chmod {
                    path: path.clone(),
                    mode: parse_mode(index, "chmod", mode)?,
                }),
                ManifestOp::Symlink { target, link } => out.push(MutatorOp::Symlink {
                    target: target.clone(),
                    link: link.clone(),
                }),
            }
        }
        Ok(out)
    }
}

fn parse_mode(index: usize, op: &'static str, mode: &str) -> Result<u32, ProvisionError> {
    let trimmed = mode.trim();
    if trimmed.len() > 4 || !trimmed.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(ProvisionError::Op {
            index,
            op,
            message: format!("invalid octal mode {mode:?}"),
        });
    }
    u32::from_str_radix(trimmed, 8).map_err(|_| ProvisionError::Op {
        index,
        op,
        message: format!("invalid octal mode {mode:?}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> &'static str {
        r#"
schema_version = 1
name = "spice-guest"

[[ops]]
op = "mkdir-p"
path = "/usr/local/bin"

[[ops]]
op = "upload-file"
guest_path = "/usr/local/bin/spice-agent"
host_path = "./bin/spice-agent"
mode = "0755"

[[ops]]
op = "write-file"
path = "/etc/spice.conf"
content = "enabled = true\n"
mode = "0644"

[[ops]]
op = "symlink"
target = "/usr/local/bin/spice-agent"
link = "/usr/bin/spice-agent"

[[ops]]
op = "chmod"
path = "/usr/bin/spice-agent"
mode = "4755"
"#
    }

    #[test]
    fn parse_accepts_every_op_kind() {
        let manifest = ProvisionManifest::parse(sample()).unwrap();
        assert_eq!(manifest.name, "spice-guest");
        assert_eq!(manifest.ops.len(), 5);
    }

    #[test]
    fn parse_rejects_unknown_schema_version() {
        let err = ProvisionManifest::parse("schema_version = 2\nops = []").unwrap_err();
        assert!(err.to_string().contains("schema_version 2"), "err: {err}");
    }

    #[test]
    fn parse_rejects_unknown_op_kind() {
        let err = ProvisionManifest::parse(
            "schema_version = 1\n[[ops]]\nop = \"nuke-from-orbit\"\npath = \"/\"",
        )
        .unwrap_err();
        assert!(err.to_string().contains("nuke-from-orbit"), "err: {err}");
    }

    #[test]
    fn parse_rejects_unknown_fields() {
        let err = ProvisionManifest::parse(
            "schema_version = 1\n[[ops]]\nop = \"rm-rf\"\npath = \"/x\"\nsudo = true",
        )
        .unwrap_err();
        assert!(err.to_string().contains("sudo"), "err: {err}");
    }

    #[test]
    fn expand_orders_mode_as_trailing_chmod() {
        let manifest = ProvisionManifest::parse(sample()).unwrap();
        let ops = manifest.to_mutator_ops().unwrap();
        assert_eq!(ops.len(), 7);
        assert!(matches!(&ops[0], MutatorOp::MkdirP { path } if path == "/usr/local/bin"));
        assert!(
            matches!(&ops[1], MutatorOp::UploadFile { path, host_path } if path == "/usr/local/bin/spice-agent" && host_path == std::path::Path::new("./bin/spice-agent"))
        );
        assert!(
            matches!(&ops[2], MutatorOp::Chmod { path, mode } if path == "/usr/local/bin/spice-agent" && *mode == 0o755)
        );
        assert!(
            matches!(&ops[3], MutatorOp::WriteFile { path, content } if path == "/etc/spice.conf" && content == b"enabled = true\n")
        );
        assert!(
            matches!(&ops[4], MutatorOp::Chmod { path, mode } if path == "/etc/spice.conf" && *mode == 0o644)
        );
        assert!(
            matches!(&ops[5], MutatorOp::Symlink { target, link } if target == "/usr/local/bin/spice-agent" && link == "/usr/bin/spice-agent")
        );
        assert!(
            matches!(&ops[6], MutatorOp::Chmod { path, mode } if path == "/usr/bin/spice-agent" && *mode == 0o4755)
        );
    }

    #[test]
    fn expand_rejects_bad_mode_with_op_index() {
        let manifest = ProvisionManifest::parse(
            "schema_version = 1\n[[ops]]\nop = \"chmod\"\npath = \"/x\"\nmode = \"0x45\"",
        )
        .unwrap();
        let err = manifest.to_mutator_ops().unwrap_err();
        assert!(
            matches!(
                &err,
                ProvisionError::Op {
                    index: 0,
                    op: "chmod",
                    ..
                }
            ),
            "err: {err}"
        );
    }

    #[test]
    fn expand_rejects_overlong_mode() {
        let manifest = ProvisionManifest::parse(
            "schema_version = 1\n[[ops]]\nop = \"write-file\"\npath = \"/x\"\ncontent = \"y\"\nmode = \"06444\"",
        )
        .unwrap();
        let err = manifest.to_mutator_ops().unwrap_err();
        assert!(err.to_string().contains("06444"), "err: {err}");
    }
}
