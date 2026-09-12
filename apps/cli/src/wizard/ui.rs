use std::io::IsTerminal;

use andler_core::{ArmTranslator, AudioBackend, DisplayEngine, RenderBackend};
use andler_firmware::HardwareDefaults;

use super::WizardKind;

const RESET: &str = "\x1b[0m";

/// ANSI styling, hand-rolled and TTY-gated: the CLI has no color crate, and
/// piped output (scripts, e2e assertions) must stay plain text.
pub(crate) fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

pub(crate) fn paint(text: &str, code: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// `▸ Name` — starts one question or one group of questions.
pub(crate) fn step(title: &str) {
    println!();
    println!("{}", paint(&format!("▸ {title}"), "1;36"));
}

/// A group heading inside the advanced pass.
pub(crate) fn group(title: &str) {
    println!();
    println!("{}", paint(&format!("  {title}"), "36"));
}

pub(crate) fn note(text: &str) {
    println!("{}", paint(text, "2"));
}

pub(crate) fn success(text: &str) {
    println!("{} {text}", paint("✓", "32"));
}

pub(crate) fn warn(text: &str) {
    println!("{} {text}", paint("⚠", "33"));
}

pub(crate) fn failure(text: &str) {
    println!("{} {text}", paint("✗", "31"));
}

enum Row {
    Section(String),
    Field(String, String),
}

fn width_of(text: &str) -> usize {
    text.chars().count()
}

/// A bordered, width-fitted block of label/value rows. Widths are measured
/// from the content (long paths in particular) instead of a constant, so a
/// 90-character base-image path cannot break the frame.
pub(crate) struct Panel {
    title: String,
    rows: Vec<Row>,
}

impl Panel {
    pub(crate) fn new(title: &str) -> Self {
        Self {
            title: title.to_string(),
            rows: Vec::new(),
        }
    }

    pub(crate) fn section(&mut self, name: &str) -> &mut Self {
        self.rows.push(Row::Section(name.to_string()));
        self
    }

    pub(crate) fn field(&mut self, label: &str, value: impl Into<String>) -> &mut Self {
        self.rows.push(Row::Field(label.to_string(), value.into()));
        self
    }

    pub(crate) fn render(&self) {
        print!("{}", self.render_to_string());
    }

    /// The frame as text, so the layout (equal-width lines, no truncation of
    /// long paths) is testable without capturing stdout.
    pub(crate) fn render_to_string(&self) -> String {
        use std::fmt::Write;

        let label_width = self
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Field(label, _) => Some(width_of(label)),
                Row::Section(_) => None,
            })
            .max()
            .unwrap_or(0);

        // Interior width: enough for the widest row (or the title line) and
        // never a fixed constant — a long base-image path must not break the
        // frame, which is exactly what the old fixed-width box did.
        let body_width = self
            .rows
            .iter()
            .map(|row| match row {
                Row::Section(name) => 1 + width_of(name),
                Row::Field(_label, value) => 3 + label_width + 2 + width_of(value),
            })
            .max()
            .unwrap_or(0);
        let width = body_width.max(3 + width_of(&self.title));

        let mut out = String::new();
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "┌─ {} {}┐",
            paint(&self.title, "1"),
            "─".repeat(width.saturating_sub(3 + width_of(&self.title)))
        );
        for row in &self.rows {
            match row {
                Row::Section(name) => {
                    let _ = writeln!(
                        out,
                        "│ {}{}│",
                        paint(name, "1"),
                        " ".repeat(width.saturating_sub(1 + width_of(name)))
                    );
                }
                Row::Field(label, value) => {
                    let value_width = width.saturating_sub(5 + label_width);
                    let _ = writeln!(
                        out,
                        "│   {label}{}  {value}{}│",
                        " ".repeat(label_width - width_of(label)),
                        " ".repeat(value_width.saturating_sub(width_of(value))),
                    );
                }
            }
        }
        let _ = writeln!(out, "└{}┘", "─".repeat(width));
        out
    }
}

pub(crate) fn hardware_panel(detected: &HardwareDefaults, kind: Option<WizardKind>) {
    let mut panel = Panel::new("Hardware detected");
    panel.field("GPU render", render_label(&detected.gpu_render));
    panel.field("Display", display_label(detected.display_engine));
    panel.field("Audio", audio_label(detected.audio_server));
    if kind != Some(WizardKind::Linux) {
        panel.field("ARM", arm_label(detected.arm_translator));
    }
    let ovmf = match &detected.ovmf {
        Ok(ovmf) => ovmf.code.display().to_string(),
        Err(err) => format!("not found ({err})"),
    };
    panel.field("OVMF", ovmf);
    panel.render();
    note("These values are the defaults the wizard proposes; every one of them can be changed.");
}

pub(crate) fn render_label(backend: &RenderBackend) -> String {
    match backend {
        RenderBackend::Venus => "Venus (Vulkan 3D)".to_string(),
        RenderBackend::VirGl => "VirGL (OpenGL 3D)".to_string(),
        RenderBackend::VirtioGpu => "VirtioGPU (2D only)".to_string(),
        RenderBackend::Cpu | RenderBackend::Passthrough { .. } => {
            "CPU (software rendering)".to_string()
        }
    }
}

pub(crate) fn display_label(engine: DisplayEngine) -> String {
    match engine {
        DisplayEngine::Sdl => "SDL",
        DisplayEngine::Gtk => "GTK",
        DisplayEngine::Spice => "SPICE",
        DisplayEngine::Dbus => "D-Bus",
        DisplayEngine::None => "None (headless)",
    }
    .to_string()
}

pub(crate) fn audio_label(backend: AudioBackend) -> String {
    match backend {
        AudioBackend::Pipewire => "PipeWire",
        AudioBackend::Pulseaudio => "PulseAudio",
        AudioBackend::None => "None",
    }
    .to_string()
}

pub(crate) fn arm_label(translator: Option<ArmTranslator>) -> String {
    match translator {
        Some(ArmTranslator::Libndk) => "libndk (AMD CPU)",
        Some(ArmTranslator::Libhoudini) => "libhoudini (Intel CPU)",
        Some(ArmTranslator::None) | None => "none",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_width_tracks_the_longest_row() {
        let mut panel = Panel::new("Summary");
        panel.section("Identity");
        panel.field("Name", "vm");
        panel.field("Base image", "/very/long/path/to/a/base/image.qcow2");

        // Rendering is the assertion here: every frame line must come out the
        // same width, which is what the old fixed-width box got wrong. Escape
        // sequences are visible-width-neutral, so they are stripped first.
        let rendered = panel.render_to_string();
        let lines: Vec<String> = rendered
            .lines()
            .filter(|line| !line.is_empty())
            .map(strip_ansi)
            .collect();
        let widths: Vec<usize> = lines.iter().map(|line| width_of(line)).collect();

        assert!(widths.len() >= 5, "rendered:\n{rendered}");
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "every frame line must have the same width: {widths:?}\n{rendered}"
        );
        assert!(
            rendered.contains("/very/long/path/to/a/base/image.qcow2"),
            "the value must survive rendering:\n{rendered}"
        );
    }

    /// Visible width, with any styling removed — what the terminal shows.
    fn strip_ansi(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for next in chars.by_ref() {
                    if next == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn paint_is_plain_text_when_color_is_disabled() {
        // NO_COLOR is the documented opt-out; with it set, styling must never
        // leak escape sequences into piped output.
        std::env::set_var("NO_COLOR", "1");
        let painted = paint("hello", "1");
        let enabled = color_enabled();
        std::env::remove_var("NO_COLOR");

        assert!(!enabled, "NO_COLOR must disable styling");
        assert_eq!(painted, "hello");
    }
}
