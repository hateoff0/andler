use crate::proto;
use andler_core::MutatorOp;

pub fn provision_ops_to_proto(ops: &[MutatorOp]) -> Vec<proto::ProvisionOp> {
    ops.iter()
        .map(|op| proto::ProvisionOp {
            op: Some(match op {
                MutatorOp::WriteFile { path, content } => {
                    proto::provision_op::Op::WriteFile(proto::ProvisionWriteFile {
                        path: path.clone(),
                        content: content.clone(),
                    })
                }
                MutatorOp::UploadFile { path, host_path } => {
                    proto::provision_op::Op::UploadFile(proto::ProvisionUploadFile {
                        guest_path: path.clone(),
                        host_path: host_path.display().to_string(),
                    })
                }
                MutatorOp::MkdirP { path } => {
                    proto::provision_op::Op::MkdirP(proto::ProvisionMkdirP { path: path.clone() })
                }
                MutatorOp::CpA { src, dst } => proto::provision_op::Op::CpA(proto::ProvisionCpA {
                    src: src.clone(),
                    dst: dst.clone(),
                }),
                MutatorOp::Mv { src, dst } => proto::provision_op::Op::Mv(proto::ProvisionMv {
                    src: src.clone(),
                    dst: dst.clone(),
                }),
                MutatorOp::RmRf { path } => {
                    proto::provision_op::Op::RmRf(proto::ProvisionRmRf { path: path.clone() })
                }
                MutatorOp::Chmod { path, mode } => {
                    proto::provision_op::Op::Chmod(proto::ProvisionChmod {
                        path: path.clone(),
                        mode: *mode,
                    })
                }
                MutatorOp::Symlink { target, link } => {
                    proto::provision_op::Op::Symlink(proto::ProvisionSymlink {
                        target: target.clone(),
                        link: link.clone(),
                    })
                }
                MutatorOp::RunShell { command } => {
                    proto::provision_op::Op::RunShell(proto::ProvisionRunShell {
                        command: command.clone(),
                    })
                }
            }),
        })
        .collect()
}

pub fn provision_ops_from_proto(
    ops: &[proto::ProvisionOp],
) -> Result<Vec<MutatorOp>, crate::ConvertError> {
    ops.iter()
        .map(|op| {
            let Some(op) = &op.op else {
                return Err(crate::ConvertError::MissingField("provision.op"));
            };
            let mutator_op = match op {
                proto::provision_op::Op::WriteFile(w) => MutatorOp::WriteFile {
                    path: w.path.clone(),
                    content: w.content.clone(),
                },
                proto::provision_op::Op::UploadFile(u) => MutatorOp::UploadFile {
                    path: u.guest_path.clone(),
                    host_path: std::path::PathBuf::from(&u.host_path),
                },
                proto::provision_op::Op::MkdirP(m) => MutatorOp::MkdirP {
                    path: m.path.clone(),
                },
                proto::provision_op::Op::CpA(c) => MutatorOp::CpA {
                    src: c.src.clone(),
                    dst: c.dst.clone(),
                },
                proto::provision_op::Op::Mv(m) => MutatorOp::Mv {
                    src: m.src.clone(),
                    dst: m.dst.clone(),
                },
                proto::provision_op::Op::RmRf(r) => MutatorOp::RmRf {
                    path: r.path.clone(),
                },
                proto::provision_op::Op::Chmod(c) => MutatorOp::Chmod {
                    path: c.path.clone(),
                    mode: c.mode,
                },
                proto::provision_op::Op::Symlink(s) => MutatorOp::Symlink {
                    target: s.target.clone(),
                    link: s.link.clone(),
                },
                // Internal only: a shell command from a client would be an
                // arbitrary-execution surface the manifest format does not
                // promise, so the request path refuses it.
                proto::provision_op::Op::RunShell(_) => {
                    return Err(crate::ConvertError::ClientSuppliedShell("run_shell"))
                }
            };
            Ok(mutator_op)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ops() -> Vec<MutatorOp> {
        vec![
            MutatorOp::MkdirP {
                path: "/a".to_string(),
            },
            MutatorOp::WriteFile {
                path: "/a/b".to_string(),
                content: b"hi".to_vec(),
            },
            MutatorOp::UploadFile {
                path: "/a/c".to_string(),
                host_path: std::path::PathBuf::from("/tmp/src"),
            },
            MutatorOp::Chmod {
                path: "/a/b".to_string(),
                mode: 0o644,
            },
            MutatorOp::Symlink {
                target: "/a/b".to_string(),
                link: "/a/d".to_string(),
            },
            MutatorOp::CpA {
                src: "/a".to_string(),
                dst: "/e".to_string(),
            },
            MutatorOp::Mv {
                src: "/e".to_string(),
                dst: "/f".to_string(),
            },
            MutatorOp::RmRf {
                path: "/f".to_string(),
            },
        ]
    }

    #[test]
    fn every_op_round_trips_through_proto() {
        let ops = sample_ops();
        let proto = provision_ops_to_proto(&ops);
        let back = provision_ops_from_proto(&proto).unwrap();
        assert_eq!(back, ops);
    }

    #[test]
    fn empty_oneof_is_rejected() {
        let proto = vec![proto::ProvisionOp { op: None }];
        let err = provision_ops_from_proto(&proto).unwrap_err();
        assert!(matches!(
            err,
            crate::ConvertError::MissingField("provision.op")
        ));
    }
}
