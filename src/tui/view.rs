//! How the rail looks: its symbols and colors, and each prompt's frame for its
//! state (`docs/TUI.md` has the full vocabulary). Text in, lines out: nothing
//! here reads a key or knows a terminal.

use super::Choice;
use super::select::Select;

/// Whether the rail is drawn in color. The shapes carry the meaning without it
/// (`NO_COLOR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors(bool);

impl Colors {
    pub const ON: Colors = Colors(true);
    pub const OFF: Colors = Colors(false);

    fn paint(self, sgr: &str, text: &str) -> String {
        if self.0 && !text.is_empty() {
            format!("\x1b[{sgr}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn cyan(self, text: &str) -> String {
        self.paint("36", text)
    }

    fn green(self, text: &str) -> String {
        self.paint("32", text)
    }

    fn red(self, text: &str) -> String {
        self.paint("31", text)
    }

    fn gray(self, text: &str) -> String {
        self.paint("90", text)
    }

    fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }

    /// Dim and struck through: a choice a person walked away from.
    fn struck(self, text: &str) -> String {
        self.paint("2;9", text)
    }
}

const BAR_START: &str = "┌";
const BAR: &str = "│";
const BAR_END: &str = "└";
const ASKING: &str = "◆";
const ANSWERED: &str = "◇";
const LEFT: &str = "■";
const HIGHLIGHTED: &str = "●";
const NOT_HIGHLIGHTED: &str = "○";

/// `┌  title`, and the rail's first stretch.
pub fn intro(title: &str, colors: Colors) -> Vec<String> {
    vec![
        format!("{}  {title}", colors.gray(BAR_START)),
        colors.gray(BAR),
    ]
}

/// `└  message`: what was decided.
pub fn outro(message: &str, colors: Colors) -> Vec<String> {
    vec![format!("{}  {message}", colors.gray(BAR_END))]
}

/// `└  message` in red: why nothing was.
pub fn cancel(message: &str, colors: Colors) -> Vec<String> {
    vec![format!("{}  {}", colors.gray(BAR_END), colors.red(message))]
}

/// A select prompt's frame. Open, it shows every choice, with the highlighted
/// one's hint; answered, it folds to the answer; left, to the choice it was on,
/// struck through.
pub fn select(prompt: &str, choices: &[Choice], state: Select, colors: Colors) -> Vec<String> {
    match state {
        Select::Open { highlighted } => {
            let bar = colors.cyan(BAR);
            let mut lines = vec![format!("{}  {prompt}", colors.cyan(ASKING))];
            for (n, choice) in choices.iter().enumerate() {
                lines.push(if n == highlighted {
                    let hint = match choice.hint.as_str() {
                        "" => String::new(),
                        hint => format!(" {}", colors.dim(&format!("({hint})"))),
                    };
                    format!(
                        "{bar}  {} {}{hint}",
                        colors.green(HIGHLIGHTED),
                        choice.label
                    )
                } else {
                    format!(
                        "{bar}  {} {}",
                        colors.dim(NOT_HIGHLIGHTED),
                        colors.dim(&choice.label)
                    )
                });
            }
            lines.push(colors.cyan(BAR_END));
            lines
        }
        Select::Answered { chosen } => {
            let bar = colors.gray(BAR);
            vec![
                format!("{}  {prompt}", colors.green(ANSWERED)),
                format!("{bar}  {}", colors.dim(&choices[chosen].label)),
                bar,
            ]
        }
        Select::Left { highlighted } => {
            let bar = colors.red(BAR);
            vec![
                format!("{}  {prompt}", colors.red(LEFT)),
                format!("{bar}  {}", colors.struck(&choices[highlighted].label)),
                bar,
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> [Choice; 3] {
        [
            Choice::new("agent-usage", "percentages"),
            Choice::new("ccusage", "token counts"),
            Choice::new("none", ""),
        ]
    }

    /// The text a person reads, colors aside.
    fn plain(line: &str) -> String {
        let mut out = String::new();
        let mut rest = line;
        while let Some(start) = rest.find("\x1b[") {
            out.push_str(&rest[..start]);
            let after = &rest[start..];
            rest = &after[after.find('m').map_or(after.len(), |end| end + 1)..];
        }
        out + rest
    }

    #[test]
    fn an_open_select_shows_every_choice_and_the_highlighted_ones_hint() {
        let frame = select(
            "Which?",
            &choices(),
            Select::Open { highlighted: 1 },
            Colors::OFF,
        );
        assert_eq!(
            frame,
            [
                "◆  Which?",
                "│  ○ agent-usage",
                "│  ● ccusage (token counts)",
                "│  ○ none",
                "└",
            ]
        );
        let on_none = select(
            "Which?",
            &choices(),
            Select::Open { highlighted: 2 },
            Colors::OFF,
        );
        assert_eq!(on_none[3], "│  ● none", "no hint, no parentheses");
    }

    #[test]
    fn an_answered_select_folds_to_its_answer_and_a_left_one_strikes_it() {
        assert_eq!(
            select(
                "Which?",
                &choices(),
                Select::Answered { chosen: 1 },
                Colors::OFF
            ),
            ["◇  Which?", "│  ccusage", "│"]
        );
        assert_eq!(
            select(
                "Which?",
                &choices(),
                Select::Left { highlighted: 0 },
                Colors::OFF
            ),
            ["■  Which?", "│  agent-usage", "│"]
        );
    }

    #[test]
    fn the_rail_opens_with_a_title_and_closes_with_what_was_decided() {
        assert_eq!(
            intro("cahoots install", Colors::OFF),
            ["┌  cahoots install", "│"]
        );
        assert_eq!(
            outro("Usage meter: ccusage", Colors::OFF),
            ["└  Usage meter: ccusage"]
        );
        assert_eq!(
            cancel("No meter chosen", Colors::OFF),
            ["└  No meter chosen"]
        );
    }

    #[test]
    fn colors_paint_the_shapes_and_leave_the_text_as_it_reads() {
        let states = [
            Select::Open { highlighted: 0 },
            Select::Answered { chosen: 1 },
            Select::Left { highlighted: 2 },
        ];
        for state in states {
            let colored = select("Which?", &choices(), state, Colors::ON);
            let plain_text: Vec<String> = colored.iter().map(|line| plain(line)).collect();
            assert_eq!(plain_text, select("Which?", &choices(), state, Colors::OFF));
        }
        let open = select(
            "Which?",
            &choices(),
            Select::Open { highlighted: 0 },
            Colors::ON,
        );
        assert_eq!(
            open[0], "\x1b[36m◆\x1b[0m  Which?",
            "the question being asked is cyan"
        );
        assert!(
            open[1].contains("\x1b[32m●\x1b[0m"),
            "the highlight is green: {:?}",
            open[1]
        );
        let left = select(
            "Which?",
            &choices(),
            Select::Left { highlighted: 2 },
            Colors::ON,
        );
        assert!(
            left[1].contains("\x1b[2;9mnone\x1b[0m"),
            "struck through: {:?}",
            left[1]
        );
        assert_eq!(
            cancel("No meter chosen", Colors::ON)[0],
            "\x1b[90m└\x1b[0m  \x1b[31mNo meter chosen\x1b[0m"
        );
    }
}
