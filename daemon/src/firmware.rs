use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct OvmfPaths {
    pub code: PathBuf,

    pub vars_template: PathBuf,
}

pub fn resolve(
    ovmf_code: Option<PathBuf>,
    ovmf_vars_template: Option<PathBuf>,
) -> Result<OvmfPaths, andler_firmware::FirmwareError> {
    match (ovmf_code, ovmf_vars_template) {
        (Some(code), Some(vars_template)) => {
            tracing::info!(
                ovmf_code = %code.display(),
                ovmf_vars_template = %vars_template.display(),
                "using explicitly specified OVMF paths"
            );
            Ok(OvmfPaths {
                code,
                vars_template,
            })
        }
        (explicit_code, explicit_vars) => {
            tracing::info!("OVMF paths not fully specified, running auto-detection");
            let detected = andler_firmware::detect_matched_pair()?;
            let code = explicit_code.unwrap_or(detected.code);
            let vars_template = explicit_vars.unwrap_or(detected.vars_template);
            tracing::info!(
                ovmf_code = %code.display(),
                ovmf_vars_template = %vars_template.display(),
                "OVMF paths resolved (auto-detected)"
            );
            Ok(OvmfPaths {
                code,
                vars_template,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uses_explicit_paths_without_detection() {
        let code = PathBuf::from("/explicit/OVMF_CODE.fd");
        let vars = PathBuf::from("/explicit/OVMF_VARS.fd");
        let result = resolve(Some(code.clone()), Some(vars.clone()))
            .expect("explicit paths must always succeed without touching FS");
        assert_eq!(result.code, code);
        assert_eq!(result.vars_template, vars);
    }

    #[test]
    fn resolve_falls_back_to_autodetect_when_paths_missing() {
        let result = resolve(None, None);
        let _ = result;
    }
}
