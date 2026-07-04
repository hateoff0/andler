//! NVIDIA GPU metrics.
//!
//! Предпочитает NVML (`nvidia-ml-wrapper`, обёртка над `libnvidia-ml.so`/
//! `nvml.dll`, той же библиотекой, поверх которой построен и сам
//! `nvidia-smi`) — прямой структурированный доступ без парсинга текста,
//! см. PLAN.md, раздел "NVIDIA metrics". `nvidia-smi` CLI остаётся
//! фоллбэком на случай, если NVML не проинициализировался (библиотеки
//! нет, недостаточно прав и т.п.) — тот же CSV-парсинг, что был здесь до
//! этого изменения, только под другим именем (`read_via_nvidia_smi`).
//!
//! `nvml-wrapper` подгружает `libnvidia-ml.so` динамически через
//! `libloading` в рантайме, не линкуется на этапе сборки — поэтому
//! наличие NVIDIA-драйвера не требуется ни для сборки этого крейта, ни
//! для его работы на хостах без NVIDIA GPU: `Nvml::init()` в этом случае
//! просто возвращает `Err`, что здесь трактуется так же, как отсутствие
//! `nvidia-smi` в PATH — молчаливый переход к следующему варианту, не
//! ошибка и не паника.

use std::sync::OnceLock;

use andler_core::ResourceMetrics;
use nvml_wrapper::Nvml;

/// Кэширует единственный инстанс `Nvml` на весь процесс `andlerd` — сама
/// библиотека рекомендует инициализироваться один раз, а не при каждом
/// опросе метрик (по умолчанию — раз в секунду на инстанс с GPU, см.
/// `metrics::DEFAULT_POLL_INTERVAL`): инициализация подгружает и
/// резолвит символы динамической библиотеки, это не бесплатно при
/// вызове на каждый тик поллера. `OnceLock`, не `Lazy`/`lazy_static` —
/// в проекте уже есть `std::sync::OnceLock` из std, отдельная
/// зависимость не нужна.
///
/// `None` внутри `Option` — значит `Nvml::init()` в этом процессе уже
/// пробовали и он не удался (нет драйвера, нет прав и т.п.); повторных
/// попыток инициализации в рамках жизни процесса намеренно нет — то же
/// самое рассуждение, что и у `andler_firmware::detect` в целом не
/// применяется здесь: `HardwareDefaults::detect_all()` пересчитывается
/// каждый вызов, потому что это дешёвые проверки sysfs/lspci, а
/// `Nvml::init()` — как раз тот случай, который стоит закэшировать, а не
/// пересчитывать.
fn nvml() -> Option<&'static Nvml> {
    static NVML: OnceLock<Option<Nvml>> = OnceLock::new();
    NVML.get_or_init(|| match Nvml::init() {
        Ok(nvml) => Some(nvml),
        Err(error) => {
            tracing::debug!(%error, "NVML unavailable, will try nvidia-smi CLI as fallback");
            None
        }
    })
    .as_ref()
}

/// `true`, если метрики NVIDIA GPU доступны хоть каким-то способом —
/// через NVML (предпочтительно) или через `nvidia-smi` CLI (фоллбэк).
pub(super) fn is_nvidia_available() -> bool {
    nvml().is_some() || nvidia_smi_available()
}

/// Читает метрики NVIDIA GPU — сперва пробует NVML, и только если он
/// недоступен или сам запрос к нему не удался (редко, но возможно —
/// например, GPU временно не отвечает), падает обратно на `nvidia-smi`
/// CLI. Оба пути возвращают `None`, если данных получить не удалось —
/// не паникуют и не различаются для вызывающей стороны
/// (`metrics::read_gpu_metrics`), которая в этом случае просто
/// переходит к следующему вендору по приоритету.
pub(super) fn read_nvidia_metrics() -> Option<ResourceMetrics> {
    if let Some(nvml) = nvml() {
        if let Some(metrics) = read_via_nvml(nvml) {
            return Some(metrics);
        }
    }
    read_via_nvidia_smi()
}

/// Метрики через NVML: `nvmlDeviceGetMemoryInfo`/`nvmlDeviceGetUtilizationRates`
/// на устройстве с индексом 0 (см. `metrics::mod.rs` — вся текущая схема
/// в проекте однокарточная, тот же принцип, что и у AMD/Intel вариантов
/// в этом модуле). `MemoryInfo.used`/`.total` уже в байтах — в отличие
/// от `nvidia-smi --format=csv`, который отдаёт MiB и требует ручного
/// домножения (см. `read_via_nvidia_smi`), здесь конвертация не нужна.
fn read_via_nvml(nvml: &Nvml) -> Option<ResourceMetrics> {
    let device = nvml.device_by_index(0).ok()?;
    let memory = device.memory_info().ok()?;
    let utilization = device.utilization_rates().ok()?;

    Some(ResourceMetrics {
        vram_used_bytes: Some(memory.used),
        vram_total_bytes: Some(memory.total),
        gpu_load_percent: Some((utilization.gpu as f32).clamp(0.0, 100.0)),
        ..ResourceMetrics::default()
    })
}

/// Checks if `nvidia-smi` binary is available in PATH.
fn nvidia_smi_available() -> bool {
    std::process::Command::new("nvidia-smi")
        .args(["--version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Reads NVIDIA GPU metrics via `nvidia-smi` CLI — fallback path, used
/// only when NVML isn't available (see `read_nvidia_metrics`).
///
/// Output format of `--query-gpu=memory.used,memory.total,utilization.gpu
/// --format=csv,noheader,nounits`:
/// ```text
/// 1024, 8192, 67
/// ```
/// Values: MiB, MiB, percent.
fn read_via_nvidia_smi() -> Option<ResourceMetrics> {
    let output = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,memory.total,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next()?.trim();

    // Handle comma-separated values (may have spaces after commas)
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() < 3 {
        return None;
    }

    let vram_used_mib: u64 = parts[0].parse().ok()?;
    let vram_total_mib: u64 = parts[1].parse().ok()?;
    let gpu_load: f32 = parts[2].parse().ok()?;

    Some(ResourceMetrics {
        vram_used_bytes: Some(vram_used_mib * 1024 * 1024),
        vram_total_bytes: Some(vram_total_mib * 1024 * 1024),
        gpu_load_percent: Some(gpu_load.clamp(0.0, 100.0)),
        ..ResourceMetrics::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_nvidia_available_does_not_panic() {
        // Should return false in CI/test environments without an NVIDIA
        // GPU/driver and without nvidia-smi.
        let _ = is_nvidia_available();
    }

    #[test]
    fn read_nvidia_metrics_without_nvidia_returns_none() {
        // If neither NVML nor nvidia-smi is available, should return
        // None, not panic.
        if !is_nvidia_available() {
            assert!(read_nvidia_metrics().is_none());
        }
    }

    #[test]
    fn nvml_lookup_does_not_panic_and_is_cached() {
        // Calling nvml() twice must not re-attempt initialization (and
        // must not panic) regardless of whether NVML is actually
        // available in the test environment — this is the whole point
        // of caching it behind a OnceLock.
        let first = nvml().is_some();
        let second = nvml().is_some();
        assert_eq!(first, second);
    }

    #[test]
    fn parse_nvidia_smi_output() {
        // Simulate nvidia-smi output parsing
        let stdout = "1024, 8192, 67\n";
        let line = stdout.lines().next().unwrap().trim();
        let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], "1024");
        assert_eq!(parts[1], "8192");
        assert_eq!(parts[2], "67");

        let vram_used_mib: u64 = parts[0].parse().unwrap();
        let vram_total_mib: u64 = parts[1].parse().unwrap();
        let gpu_load: f32 = parts[2].parse().unwrap();

        assert_eq!(vram_used_mib * 1024 * 1024, 1024 * 1024 * 1024);
        assert_eq!(vram_total_mib * 1024 * 1024, 8192 * 1024 * 1024);
        assert!((gpu_load - 67.0).abs() < f32::EPSILON);
    }
}
