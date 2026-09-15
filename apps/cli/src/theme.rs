use cliclack::{Theme, ThemeState};
use console::Style;

/// The cliclack theme the CLI's dialog runs with.
///
/// cliclack's own theme colours the symbols (cyan bar, green cursor, yellow
/// warnings) and leaves every *text* at the same weight, which makes a screen
/// of questions and hints one grey block. This one keeps the symbols and adds
/// the emphasis where the eye needs it: the question is bold, the answer and
/// the hints are dim, and the entry the cursor sits on is the bold one.
///
/// Styles come from `console`, so `NO_COLOR` strips all of it exactly as it
/// does everywhere else in the CLI.
struct HandlerTheme;

impl Theme for HandlerTheme {
    /// The question being asked is the bright line; a submitted prompt dims
    /// into the record of what was answered. Multi-line prompts keep the bar
    /// prefix for their continuation lines.
    fn format_header(&self, state: &ThemeState, prompt: &str) -> String {
        let text = match state {
            ThemeState::Active | ThemeState::Error(_) => Style::new().bold(),
            ThemeState::Submit | ThemeState::Cancel => Style::new().dim(),
        };
        prompt
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    format!("{}  {}\n", self.state_symbol(state), text.apply_to(line))
                } else {
                    format!(
                        "{}  {}\n",
                        self.bar_color(state).apply_to("│"),
                        text.apply_to(line)
                    )
                }
            })
            .collect()
    }

    /// The entry the cursor sits on (`input_style`) and the rest of the list.
    fn input_style(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Cancel => Style::new().dim().strikethrough(),
            ThemeState::Submit => Style::new().dim(),
            _ => Style::new().bold(),
        }
    }

    /// Hints, placeholders and unselected entries: quiet, never hidden.
    fn placeholder_style(&self, state: &ThemeState) -> Style {
        match state {
            ThemeState::Cancel => Style::new().hidden(),
            _ => Style::new().dim(),
        }
    }
}

/// Installs the theme. Called once from the CLI's entry point: it is global
/// state inside cliclack, and the spinner and progress-bar lines share it.
pub fn install() {
    cliclack::set_theme(HandlerTheme);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The theme must not change what a line *says*, only how it looks: these
    /// tests strip the styling and compare against cliclack's own rendering.
    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(character) = chars.next() {
            if character == '\u{1b}' {
                for character in chars.by_ref() {
                    if character == 'm' {
                        break;
                    }
                }
            } else {
                out.push(character);
            }
        }
        out
    }

    #[test]
    fn a_question_keeps_its_lines_and_its_bar_prefix() {
        let theme = HandlerTheme;
        let rendered = theme.format_header(&ThemeState::Active, "network mode\nbridge or isolated");

        // Both sides stripped: whether `console` paints depends on a global
        // flag another test in this binary may have toggled, and the property
        // under test is the line structure, not the escapes.
        assert_eq!(
            strip(&rendered),
            strip(&format!(
                "{}  network mode\n│  bridge or isolated\n",
                theme.state_symbol(&ThemeState::Active)
            )),
            "styling must not move or drop a line"
        );
    }

    /// `console` hides styling when its stream is not a terminal, and a test
    /// harness is not one: the flag is forced for the length of a check and
    /// put back afterwards, panic or not.
    struct ColorsForced(bool);

    impl ColorsForced {
        fn on() -> Self {
            let previous = console::colors_enabled();
            console::set_colors_enabled(true);
            Self(previous)
        }
    }

    impl Drop for ColorsForced {
        fn drop(&mut self) {
            console::set_colors_enabled(self.0);
        }
    }

    #[test]
    fn the_emphasis_only_wraps_text_and_never_rewrites_it() {
        let _forced = ColorsForced::on();
        let theme = HandlerTheme;
        let styled = theme
            .input_style(&ThemeState::Active)
            .apply_to("venus")
            .to_string();
        assert_ne!(
            styled, "venus",
            "the entry under the cursor has to stand out from the plain ones"
        );
        assert_eq!(
            strip(&styled),
            "venus",
            "emphasis is decoration: the item still reads as itself"
        );
        // The state symbols differ by design (◆ asking, ◇ answered); what the
        // emphasis must not touch is the question itself.
        let active = strip(&theme.format_header(&ThemeState::Active, "disk size"));
        let submitted = strip(&theme.format_header(&ThemeState::Submit, "disk size"));
        assert!(active.ends_with("disk size\n") && submitted.ends_with("disk size\n"));
        assert_eq!(
            active.chars().count(),
            submitted.chars().count(),
            "only the symbol's colour changes between asking and answered"
        );
    }
}
