use std::io::{IsTerminal, Write};

use andler_core::{
    ArmTranslator, AudioBackend, CdromBus, DisplayEngine, PointerMode, RenderBackend,
};
use andler_firmware::HardwareDefaults;

use super::WizardKind;

const RESET: &str = "\x1b[0m";

const ACCENT: &str = "36";
const TITLE: &str = "1;36";
const DIM: &str = "2";
const OK: &str = "32";
const ATTENTION: &str = "33";
const ERROR: &str = "31";

const INDENT: usize = 2;
const GAP: usize = 2;
const MIN_COLUMNS: usize = 32;
const MAX_COLUMNS: usize = 100;
const SECTION_COLUMNS: usize = 80;
const MIN_RULE: usize = 4;
const STATUS_WIDTH: usize = 7;

fn forced_non_tty() -> bool {
    std::env::var_os("ANDLER_WIZARD_NOT_TTY").is_some()
}

fn color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && !forced_non_tty() && std::io::stdout().is_terminal()
}

fn paint(text: &str, code: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}{RESET}")
    } else {
        text.to_string()
    }
}

fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut escape = false;
    for c in text.chars() {
        match (escape, c) {
            (false, '\u{1b}') => escape = true,
            (true, 'm') => escape = false,
            (true, _) => {}
            (false, _) => width += 1,
        }
    }
    width
}

fn pad(text: &str, width: usize) -> String {
    let mut padded = text.to_string();
    padded.push_str(&" ".repeat(width.saturating_sub(visible_width(text))));
    padded
}

fn wrap(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut line_width = 0;

    for word in text.split_whitespace() {
        let separator = usize::from(!line.is_empty());
        let word_width = word.chars().count();
        if line_width + separator + word_width <= limit {
            if separator == 1 {
                line.push(' ');
            }
            line.push_str(word);
            line_width += separator + word_width;
            continue;
        }

        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }

        let mut word = word;
        while word.chars().count() > limit {
            let cut = word
                .char_indices()
                .nth(limit)
                .map(|(index, _)| index)
                .unwrap_or(word.len());
            let (head, tail) = word.split_at(cut);
            lines.push(head.to_string());
            word = tail;
        }
        line_width = word.chars().count();
        line = word.to_string();
    }

    lines.push(line);
    lines
}

#[derive(Clone, Copy)]
pub(crate) enum Status {
    Ok,
    Present,
    Skipped,
    Unknown,
    Warn,
    Failed,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Present => "present",
            Status::Skipped => "skipped",
            Status::Unknown => "unknown",
            Status::Warn => "warn",
            Status::Failed => "failed",
        }
    }

    fn color(self) -> &'static str {
        match self {
            Status::Ok | Status::Present => OK,
            Status::Skipped | Status::Unknown | Status::Warn => ATTENTION,
            Status::Failed => ERROR,
        }
    }
}

enum Row {
    Section(String),
    Field(String, String),
    Entry(String, Status, String),
    Outcome(Status, String),
    Note(String),
}

pub(crate) struct Screen {
    rows: Vec<Row>,
}

impl Screen {
    pub(crate) fn new() -> Self {
        Self { rows: Vec::new() }
    }

    pub(crate) fn section(&mut self, name: &str) -> &mut Self {
        self.rows.push(Row::Section(name.to_string()));
        self
    }

    pub(crate) fn field(&mut self, label: &str, detail: impl Into<String>) -> &mut Self {
        self.rows.push(Row::Field(label.to_string(), detail.into()));
        self
    }

    pub(crate) fn entry(
        &mut self,
        label: &str,
        status: Status,
        detail: impl Into<String>,
    ) -> &mut Self {
        self.rows
            .push(Row::Entry(label.to_string(), status, detail.into()));
        self
    }

    pub(crate) fn outcome(&mut self, status: Status, detail: impl Into<String>) -> &mut Self {
        self.rows.push(Row::Outcome(status, detail.into()));
        self
    }

    pub(crate) fn note(&mut self, text: &str) -> &mut Self {
        self.rows.push(Row::Note(text.to_string()));
        self
    }

    pub(crate) fn print(&self) {
        print!("{}", self.render_to_string());
    }

    pub(crate) fn render_to_string(&self) -> String {
        self.render_at(screen_width())
    }

    pub(crate) fn render_at(&self, width: usize) -> String {
        let label_width = self
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Field(label, _) | Row::Entry(label, ..) => Some(visible_width(label)),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let status_width = if self.rows.iter().any(|row| matches!(row, Row::Entry(..))) {
            STATUS_WIDTH + GAP
        } else {
            0
        };

        let mut parts: Vec<Part> = Vec::with_capacity(self.rows.len());
        let mut table_width = 0;
        for row in &self.rows {
            let part = match row {
                Row::Section(name) => Part::Section(name.clone()),
                Row::Field(label, detail) => {
                    let (prefix, column) = row_prefix(label, None, label_width, status_width);
                    Part::Body(body(prefix, column, detail, width))
                }
                Row::Entry(label, status, detail) => {
                    let (prefix, column) =
                        row_prefix(label, Some(*status), label_width, status_width);
                    Part::Body(body(prefix, column, detail, width))
                }
                Row::Outcome(status, detail) => {
                    let prefix = format!(
                        "{}{}{}",
                        " ".repeat(INDENT),
                        status_word(*status),
                        " ".repeat(GAP)
                    );
                    Part::Body(body(prefix, INDENT + STATUS_WIDTH + GAP, detail, width))
                }
                Row::Note(text) => Part::Note(body(" ".repeat(INDENT), INDENT, text, width)),
            };
            if let Part::Body(text) = &part {
                table_width = table_width.max(widest_line(text));
            }
            parts.push(part);
        }

        let mut out = String::new();
        for (index, part) in parts.iter().enumerate() {
            match part {
                Part::Section(name) => {
                    if index > 0 {
                        out.push('\n');
                    }
                    out.push_str(&section_line(name, table_width));
                }
                Part::Body(text) => out.push_str(text),
                Part::Note(text) => {
                    if index > 0 && !matches!(parts[index - 1], Part::Note(_)) {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
        }
        out
    }
}

enum Part {
    Section(String),
    Body(String),
    Note(String),
}

fn widest_line(text: &str) -> usize {
    text.lines().map(visible_width).max().unwrap_or(0)
}

fn row_prefix(
    label: &str,
    status: Option<Status>,
    label_width: usize,
    status_width: usize,
) -> (String, usize) {
    let mut prefix = " ".repeat(INDENT);
    if label_width > 0 {
        prefix.push_str(&pad(label, label_width));
        prefix.push_str(&" ".repeat(GAP));
    }
    if status_width > 0 {
        match status {
            Some(status) => {
                prefix.push_str(&status_word(status));
                prefix.push_str(&" ".repeat(GAP));
            }
            None => prefix.push_str(&" ".repeat(status_width)),
        }
    }
    (prefix, INDENT + label_width + GAP + status_width)
}

fn status_word(status: Status) -> String {
    paint(&pad(status.word(), STATUS_WIDTH), status.color())
}

fn body(prefix: String, column: usize, text: &str, width: usize) -> String {
    let continuation = " ".repeat(column);
    let mut out = String::new();
    for (index, line) in wrap(text, width.saturating_sub(column)).iter().enumerate() {
        if index == 0 {
            out.push_str(&prefix);
        } else {
            out.push_str(&continuation);
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn section_line(name: &str, table_width: usize) -> String {
    let head = format!("{}{}", " ".repeat(INDENT), paint(name, ACCENT));
    let rule = table_width.saturating_sub(INDENT + visible_width(name) + GAP);
    if rule < MIN_RULE {
        return format!("{head}\n");
    }
    format!(
        "{head}{}{}\n",
        " ".repeat(GAP),
        paint(&"─".repeat(rule), DIM)
    )
}

fn screen_width() -> usize {
    if forced_non_tty() || !std::io::stdout().is_terminal() {
        return usize::MAX;
    }
    terminal_columns(libc::STDOUT_FILENO)
        .map(|columns| columns.clamp(MIN_COLUMNS, MAX_COLUMNS))
        .unwrap_or(usize::MAX)
}

fn terminal_columns(fd: i32) -> Option<usize> {
    // SAFETY: TIOCGWINSZ only writes the size into the winsize this call owns.
    let (measured, size) = unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        let measured = libc::ioctl(fd, libc::TIOCGWINSZ, &mut size);
        (measured, size)
    };
    (measured == 0 && size.ws_col > 0).then_some(size.ws_col as usize)
}

pub(crate) fn header(title: &str) {
    println!();
    println!("{}", paint(&format!("▸ {title}"), TITLE));
}

pub(crate) fn section(title: &str) {
    println!();
    print!("{}", section_line(title, section_width()));
}

fn section_width() -> usize {
    screen_width().min(SECTION_COLUMNS)
}

pub(crate) fn note(text: &str) {
    let mut screen = Screen::new();
    screen.note(text);
    screen.print();
}

pub(crate) fn result(status: Status, text: &str) {
    let mut screen = Screen::new();
    screen.outcome(status, text);
    screen.print();
}

pub(crate) struct Progress {
    live: bool,
    columns: usize,
    reported: String,
}

impl Progress {
    pub(crate) fn start(label: &str) -> Self {
        let live = std::io::stderr().is_terminal() && !forced_non_tty();
        let mut progress = Self {
            live,
            columns: terminal_columns(libc::STDERR_FILENO).unwrap_or(MAX_COLUMNS),
            reported: String::new(),
        };
        progress.show(label);
        progress
    }

    pub(crate) fn update(&mut self, line: &str) {
        self.show(line);
    }

    pub(crate) fn finish(&mut self) {
        if std::mem::replace(&mut self.live, false) {
            erase_line();
        }
    }

    fn show(&mut self, line: &str) {
        if self.live {
            let mut stderr = std::io::stderr();
            let _ = write!(stderr, "\r{}\u{1b}[K", fit(line, self.columns));
            let _ = stderr.flush();
        } else if line != self.reported {
            eprintln!("{}{line}", " ".repeat(INDENT));
        }
        self.reported.clear();
        self.reported.push_str(line);
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

fn erase_line() {
    let mut stderr = std::io::stderr();
    let _ = write!(stderr, "\r\u{1b}[K");
    let _ = stderr.flush();
}

fn fit(line: &str, columns: usize) -> String {
    if visible_width(line) <= columns {
        return line.to_string();
    }
    let mut fitted: String = line.chars().take(columns.saturating_sub(1)).collect();
    fitted.push('\u{2026}');
    fitted
}

pub(crate) fn hardware_screen(detected: &HardwareDefaults, kind: Option<WizardKind>) {
    header("hardware");

    let mut screen = Screen::new();
    screen.field("gpu render", render_label(&detected.gpu_render));
    screen.field("display", display_label(detected.display_engine));
    screen.field("audio", audio_label(detected.audio_server));
    if kind != Some(WizardKind::Linux) {
        screen.field("arm", arm_label(detected.arm_translator));
    }
    let ovmf = match &detected.ovmf {
        Ok(ovmf) => ovmf.code.display().to_string(),
        Err(err) => format!("not found ({err})"),
    };
    screen.field("ovmf", ovmf);
    screen.note(
        "These values are the defaults the wizard proposes; every one of them can be changed.",
    );
    screen.print();
}

pub(crate) fn render_label(backend: &RenderBackend) -> &'static str {
    match backend {
        RenderBackend::Venus => "Venus (Vulkan 3D)",
        RenderBackend::VirGl => "VirGL (OpenGL 3D)",
        RenderBackend::VirtioGpu => "VirtioGPU (2D only)",
        RenderBackend::Cpu | RenderBackend::Passthrough { .. } => "CPU (software rendering)",
    }
}

pub(crate) fn display_label(engine: DisplayEngine) -> &'static str {
    match engine {
        DisplayEngine::Sdl => "SDL",
        DisplayEngine::Gtk => "GTK",
        DisplayEngine::Spice => "SPICE",
        DisplayEngine::Dbus => "D-Bus",
        DisplayEngine::None => "None (headless)",
    }
}

pub(crate) fn audio_label(backend: AudioBackend) -> &'static str {
    match backend {
        AudioBackend::Pipewire => "PipeWire",
        AudioBackend::Pulseaudio => "PulseAudio",
        AudioBackend::None => "None",
    }
}

pub(crate) fn arm_label(translator: Option<ArmTranslator>) -> &'static str {
    match translator {
        Some(ArmTranslator::Libndk) => "libndk (AMD CPU)",
        Some(ArmTranslator::Libhoudini) => "libhoudini (Intel CPU)",
        Some(ArmTranslator::None) | None => "none",
    }
}

pub(crate) fn cdrom_bus_label(bus: CdromBus) -> &'static str {
    match bus {
        CdromBus::VirtioScsi => "virtio-scsi",
        CdromBus::Ide => "ide",
    }
}

pub(crate) fn pointer_label(pointer: PointerMode) -> &'static str {
    match pointer {
        PointerMode::Tablet => "tablet",
        PointerMode::Mouse => "mouse",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column_of(rendered: &str, needle: &str) -> usize {
        rendered
            .lines()
            .find(|line| line.contains(needle))
            .and_then(|line| line.find(needle))
            .unwrap_or_else(|| panic!("{needle:?} is missing from:\n{rendered}"))
    }

    #[test]
    fn details_line_up_in_one_column() {
        let mut screen = Screen::new();
        screen.field("name", "my-linux");
        screen.field("base image", "/cache/base.qcow2");

        let rendered = screen.render_at(80);

        assert_eq!(
            column_of(&rendered, "my-linux"),
            column_of(&rendered, "/cache/base.qcow2"),
            "{rendered}"
        );
    }

    #[test]
    fn a_status_word_sits_between_its_label_and_detail() {
        let mut screen = Screen::new();
        screen.field("name", "my-android");
        screen.entry("spice-vdagent", Status::Ok, "installed");
        screen.entry("arm-translator", Status::Present, "already there");

        let rendered = screen.render_at(80);

        assert_eq!(
            column_of(&rendered, "installed"),
            column_of(&rendered, "my-android"),
            "{rendered}"
        );
        assert_eq!(
            column_of(&rendered, "already there"),
            column_of(&rendered, "installed"),
            "{rendered}"
        );
        assert!(rendered.contains("ok "), "{rendered}");
        assert!(rendered.contains("present "), "{rendered}");
    }

    #[test]
    fn an_outcome_for_the_whole_step_starts_at_the_margin() {
        let mut screen = Screen::new();
        screen.section("installed in the guest");
        screen.outcome(Status::Failed, "the daemon is not running");

        let rendered = screen.render_at(80);

        assert!(
            rendered.lines().any(|line| line.starts_with("  failed ")),
            "{rendered}"
        );
    }

    #[test]
    fn wrapped_details_stay_inside_the_width_and_keep_every_character() {
        let path = "/home/user/.andler/instances/0123456789abcdef/disk.qcow2";
        let mut screen = Screen::new();
        screen.section("storage");
        screen.field("disk", path);

        let rendered = screen.render_at(80);

        for line in rendered.lines() {
            assert!(visible_width(line) <= 80, "line overflows: {line:?}");
        }
        let flattened: String = rendered
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>();
        let expected: String = path.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(flattened.contains(&expected), "{rendered}");
        assert!(rendered.contains('─'), "{rendered}");
    }

    #[test]
    fn status_words_fit_the_status_column() {
        for status in [
            Status::Ok,
            Status::Present,
            Status::Skipped,
            Status::Unknown,
            Status::Warn,
            Status::Failed,
        ] {
            assert!(
                status.word().len() <= STATUS_WIDTH,
                "{} does not fit the status column",
                status.word()
            );
        }
    }

    #[test]
    fn styling_is_plain_text_when_color_is_disabled() {
        std::env::set_var("NO_COLOR", "1");
        let painted = paint("hello", "1");
        let mut screen = Screen::new();
        screen.entry("spice-vdagent", Status::Ok, "installed");
        let rendered = screen.render_to_string();
        let enabled = color_enabled();
        std::env::remove_var("NO_COLOR");

        assert!(!enabled, "NO_COLOR must disable styling");
        assert_eq!(painted, "hello");
        assert!(
            !rendered.contains('\u{1b}'),
            "no escape sequence may reach a styled-off screen: {rendered:?}"
        );
    }
}
