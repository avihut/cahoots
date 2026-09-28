//! How a command's end looks: its blocks on the rail, its last word, and
//! then the lines to copy, flush left. Text is wrapped at `WIDTH` columns
//! without breaking a `command` in backticks. A control character in any of
//! it is shown as its escape, never sent to the terminal: some of this text
//! was written by an agent, and a byte like ESC could move the cursor or
//! retitle the terminal.

use super::super::ending::{Block, Checked, Ending, Item, Last, Mark};
use super::super::page::Origin;
use super::{ANSWERED, BAR, BAR_END, Colors, FAILED, INFO, WARNING, intro};

/// The columns an ending is wrapped at. Reading the terminal's own width
/// takes unsafe code or a question to the terminal; 80 fits any terminal.
pub const WIDTH: usize = 80;
/// A mark, or the rail, and the two spaces after it.
const INDENT: usize = 3;
/// The narrowest a listing's values are wrapped to, however long its labels.
const NARROWEST: usize = 20;

/// The ending as lines of text.
pub fn lines(ending: &Ending, colors: Colors) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(title) = &ending.title {
        out.extend(intro(&printable(title), colors));
    }
    let bar = format!("{}  ", colors.gray(BAR));
    // Every listing's values in one column, as on the settings page.
    let label_width = ending
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Listing { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .map(|item| printable(&item.label).chars().count())
        .max()
        .unwrap_or(0);
    for block in &ending.blocks {
        match block {
            Block::Said { mark, text, lines } => {
                let mark = format!("{}  ", painted(*mark, colors));
                hang(&mut out, &mark, &bar, text, |text| text.to_string());
                for line in lines {
                    hang(&mut out, &bar, &bar, line, |text| text.to_string());
                }
            }
            Block::Lines(lines) => {
                for line in lines {
                    hang(&mut out, &bar, &bar, line, |text| text.to_string());
                }
            }
            Block::Listing { title, items } => {
                let mark = format!("{}  ", colors.green(ANSWERED));
                hang(&mut out, &mark, &bar, title, |text| text.to_string());
                listing(&mut out, &bar, items, label_width, colors);
            }
            Block::Checks(rows) => checks(&mut out, &bar, rows, colors),
        }
        out.push(colors.gray(BAR));
    }
    let end = format!("{}  ", colors.gray(BAR_END));
    let under = " ".repeat(INDENT);
    match &ending.last {
        Last::Said(message) => hang(&mut out, &end, &under, message, |text| text.to_string()),
        Last::Refused(message) => hang(&mut out, &end, &under, message, |text| colors.red(text)),
    }
    for paste in &ending.paste {
        out.push(String::new());
        out.push(printable(&paste.heading));
        out.extend(paste.lines.iter().map(|line| printable(line)));
    }
    out
}

/// `text` wrapped, its first line after `first` and the rest after `rest`
/// (each as wide as `INDENT`), each piece painted by `paint`.
fn hang(
    out: &mut Vec<String>,
    first: &str,
    rest: &str,
    text: &str,
    paint: impl Fn(&str) -> String,
) {
    for (n, piece) in wrap(&printable(text), WIDTH - INDENT).iter().enumerate() {
        let before = if n == 0 { first } else { rest };
        out.push(format!("{before}{}", paint(piece)));
    }
}

/// A label and a value on each line: the values in a column of their own,
/// after labels `label_width` wide, `•` just before one that is set, and a
/// long one wrapped under itself.
fn listing(out: &mut Vec<String>, bar: &str, items: &[Item], label_width: usize, colors: Colors) {
    let labels: Vec<String> = items.iter().map(|item| printable(&item.label)).collect();
    // The label, two spaces, and the mark's two columns.
    let before_value = label_width + 4;
    let room = (WIDTH - INDENT).saturating_sub(before_value).max(NARROWEST);
    for (item, label) in items.iter().zip(&labels) {
        let pad = " ".repeat(label_width - label.chars().count() + 2);
        let mark = if item.origin == Origin::Set {
            "• "
        } else {
            "  "
        };
        let paint = |text: &str| match item.origin {
            Origin::Set => text.to_string(),
            Origin::Default | Origin::Found => colors.dim(text),
        };
        let pieces = wrap(&printable(&item.value), room);
        if pieces.is_empty() {
            out.push(format!("{bar}{label}"));
        }
        for (n, piece) in pieces.iter().enumerate() {
            out.push(if n == 0 {
                format!("{bar}{label}{pad}{mark}{}", paint(piece))
            } else {
                format!("{bar}{}{}", " ".repeat(before_value), paint(piece))
            });
        }
    }
}

/// A mark in its color.
fn painted(mark: Mark, colors: Colors) -> String {
    match mark {
        Mark::Done => colors.green(ANSWERED),
        Mark::Info => colors.blue(INFO),
        Mark::Warning => colors.yellow(WARNING),
        Mark::Failed => colors.red(FAILED),
    }
}

/// A mark, a label and a text on each line: the texts in a column of their
/// own, after the widest label, and a long one wrapped under itself.
fn checks(out: &mut Vec<String>, bar: &str, rows: &[Checked], colors: Colors) {
    let labels: Vec<String> = rows.iter().map(|row| printable(&row.label)).collect();
    let label_width = labels
        .iter()
        .map(|label| label.chars().count())
        .max()
        .unwrap_or(0);
    // The label and two spaces.
    let before_text = label_width + 2;
    let room = (WIDTH - INDENT).saturating_sub(before_text).max(NARROWEST);
    for (row, label) in rows.iter().zip(&labels) {
        let first = format!("{}  {label}", painted(row.mark, colors));
        let pieces = wrap(&printable(&row.text), room);
        match pieces.split_first() {
            None => out.push(first),
            Some((head, rest)) => {
                let pad = " ".repeat(before_text - label.chars().count());
                out.push(format!("{first}{pad}{head}"));
                for piece in rest {
                    out.push(format!("{bar}{}{piece}", " ".repeat(before_text)));
                }
            }
        }
    }
}

/// `text` as it may be shown: each control character as its escape.
fn printable(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() {
                ch.escape_unicode().to_string()
            } else {
                ch.to_string()
            }
        })
        .collect()
}

/// The words of `text` in lines at most `width` wide. A `command` in
/// backticks is one word however many it has, so it is never broken, and a
/// word wider than a line has a line to itself.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut in_code = false;
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        match words.last_mut() {
            Some(last) if in_code => {
                last.push(' ');
                last.push_str(word);
            }
            _ => words.push(word.to_string()),
        }
        if word.matches('`').count() % 2 == 1 {
            in_code = !in_code;
        }
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in words {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::super::super::ending::Paste;
    use super::*;

    fn said(title: Option<&str>, blocks: Vec<Block>, last: Last) -> Vec<String> {
        lines(
            &Ending {
                title: title.map(str::to_string),
                blocks,
                last,
                paste: Vec::new(),
            },
            Colors::OFF,
        )
    }

    #[test]
    fn a_one_line_end_is_the_title_the_rail_and_the_last_word() {
        assert_eq!(
            said(
                Some("cahoots enable"),
                Vec::new(),
                Last::Said("Saved to config.toml: [harness.codex] enabled = true".into())
            ),
            [
                "┌  cahoots enable",
                "│",
                "└  Saved to config.toml: [harness.codex] enabled = true",
            ]
        );
    }

    #[test]
    fn each_block_is_followed_by_the_rail_and_an_open_rail_is_gone_on_from() {
        let blocks = vec![
            Block::done("Installed", vec!["~/first/skills/cahoots/SKILL.md".into()]),
            Block::info("Usage meter: ccusage.", vec!["Saved to config.toml".into()]),
            Block::warning("Something is left to do.", Vec::new()),
            Block::Lines(vec!["one".into(), "two".into()]),
        ];
        assert_eq!(
            said(None, blocks, Last::Said("Done".into())),
            [
                "◇  Installed",
                "│  ~/first/skills/cahoots/SKILL.md",
                "│",
                "●  Usage meter: ccusage.",
                "│  Saved to config.toml",
                "│",
                "▲  Something is left to do.",
                "│",
                "│  one",
                "│  two",
                "│",
                "└  Done",
            ],
            "no title: a question already opened the rail"
        );
    }

    #[test]
    fn text_wraps_at_80_columns_and_a_command_in_backticks_is_never_broken() {
        let text = "Add the rules below yourself: cahoots never edits a harness's permissions. \
                    Then `cahoots enable <harness>` for each target you want, and \
                    `cahoots doctor` to check.";
        let shown = said(None, Vec::new(), Last::Said(text.into()));
        assert_eq!(
            shown,
            [
                "└  Add the rules below yourself: cahoots never edits a harness's permissions.",
                "   Then `cahoots enable <harness>` for each target you want, and",
                "   `cahoots doctor` to check.",
            ]
        );
        assert!(shown.iter().all(|line| line.chars().count() <= WIDTH));
        let warned = said(
            None,
            vec![Block::warning("word ".repeat(30), Vec::new())],
            Last::Said("x".into()),
        );
        assert!(warned[0].starts_with("▲  word") && warned[1].starts_with("│  word"));
        assert!(warned.iter().all(|line| line.chars().count() <= WIDTH));
    }

    #[test]
    fn a_refusal_is_the_last_word_in_red_and_colors_change_nothing_else() {
        let ending = Ending {
            title: Some("cahoots settings".into()),
            blocks: vec![
                Block::done("Done", vec!["a line".into()]),
                Block::info("Info", Vec::new()),
                Block::warning("Warning", Vec::new()),
            ],
            last: Last::Refused("harness.codex.cap = 101: 1–100".into()),
            paste: Vec::new(),
        };
        let colored = lines(&ending, Colors::ON);
        assert_eq!(
            colored.last().unwrap(),
            "\x1b[90m└\x1b[0m  \x1b[31mharness.codex.cap = 101: 1–100\x1b[0m"
        );
        assert!(colored.iter().any(|line| line == "\x1b[32m◇\x1b[0m  Done"));
        assert!(colored.iter().any(|line| line == "\x1b[34m●\x1b[0m  Info"));
        assert!(
            colored
                .iter()
                .any(|line| line == "\x1b[33m▲\x1b[0m  Warning")
        );
        let plain: Vec<String> = colored.iter().map(|line| strip(line)).collect();
        assert_eq!(plain, lines(&ending, Colors::OFF));
    }

    #[test]
    fn a_listing_puts_values_in_a_column_marks_what_is_set_and_wraps_under_itself() {
        let item = |label: &str, value: &str, origin| Item {
            label: label.into(),
            value: value.into(),
            origin,
        };
        let long = "claude opus high, then codex gpt-6-astra high, then claude sonnet medium, \
                    then codex gpt-5.6-terra medium";
        let shown = said(
            None,
            vec![Block::Listing {
                title: "Codex".into(),
                items: vec![
                    item("Enabled", "on", Origin::Set),
                    item("Usage cap", "75%", Origin::Default),
                    item("Program", "~/bin/codex", Origin::Found),
                    item("Advise", long, Origin::Default),
                ],
            }],
            Last::Said("• set in config.toml".into()),
        );
        assert_eq!(
            shown,
            [
                "◇  Codex",
                "│  Enabled    • on",
                "│  Usage cap    75%",
                "│  Program      ~/bin/codex",
                "│  Advise       claude opus high, then codex gpt-6-astra high, then claude",
                "│               sonnet medium, then codex gpt-5.6-terra medium",
                "│",
                "└  • set in config.toml",
            ]
        );
        let dim = said_colored(vec![Block::Listing {
            title: "T".into(),
            items: vec![
                item("Set", "on", Origin::Set),
                item("Default", "off", Origin::Default),
            ],
        }]);
        assert!(dim[1].ends_with("• on"), "{:?}", dim[1]);
        assert!(dim[2].ends_with("\x1b[2moff\x1b[0m"), "{:?}", dim[2]);
        let two = said(
            None,
            vec![
                Block::Listing {
                    title: "Short".into(),
                    items: vec![item("Cap", "75%", Origin::Default)],
                },
                Block::Listing {
                    title: "Long".into(),
                    items: vec![item("Delegation depth", "1", Origin::Set)],
                },
            ],
            Last::Said("x".into()),
        );
        assert_eq!(
            two[1..5],
            [
                "│  Cap                 75%",
                "│",
                "◇  Long",
                "│  Delegation depth  • 1",
            ],
            "one column for every listing"
        );
    }

    #[test]
    fn checks_put_each_mark_by_its_label_and_every_text_in_one_column() {
        let row = |mark, label: &str, text: &str| Checked {
            mark,
            label: label.into(),
            text: text.into(),
        };
        let rows = vec![
            row(Mark::Done, "build", "1.2.3"),
            row(
                Mark::Warning,
                "first: caller rules",
                "to let first delegate without a prompt, add the rules below, which a person \
                 pastes by hand",
            ),
            row(Mark::Failed, "config", ""),
        ];
        let last = "3 checks: 1 passed, 1 warning, 1 failed";
        // The widest label, then two spaces: the texts start 21 columns in.
        let column = |label: &str| " ".repeat(21 - label.chars().count());
        assert_eq!(
            said(
                Some("cahoots doctor"),
                vec![Block::Checks(rows.clone())],
                Last::Refused(last.into())
            ),
            [
                "┌  cahoots doctor".to_string(),
                "│".to_string(),
                format!("◇  build{}1.2.3", column("build")),
                format!(
                    "▲  first: caller rules{}to let first delegate without a prompt, add the rules",
                    column("first: caller rules")
                ),
                format!("│  {}below, which a person pastes by hand", column("")),
                "■  config".to_string(),
                "│".to_string(),
                format!("└  {last}"),
            ]
        );
        let colored = said_colored(vec![Block::Checks(rows)]);
        assert!(colored[0].starts_with("\x1b[32m◇\x1b[0m  build"));
        assert!(colored[1].starts_with("\x1b[33m▲\x1b[0m  first: caller rules"));
        assert_eq!(colored[3], "\x1b[31m■\x1b[0m  config");
    }

    fn said_colored(blocks: Vec<Block>) -> Vec<String> {
        lines(
            &Ending {
                title: None,
                blocks,
                last: Last::Said("x".into()),
                paste: Vec::new(),
            },
            Colors::ON,
        )
    }

    #[test]
    fn lines_to_copy_come_after_the_rail_flush_left_and_are_never_wrapped() {
        let long = format!("prefix_rule(pattern=[\"cahoots\", \"{}\"])", "x".repeat(90));
        let ending = Ending {
            title: Some("cahoots install".into()),
            blocks: Vec::new(),
            last: Last::Said("Add the rules below yourself.".into()),
            paste: vec![
                Paste {
                    heading: "Add to ~/first/settings.json (permissions.allow):".into(),
                    lines: vec![
                        "\"Bash(cahoots pick:*)\",".into(),
                        "\"Bash(cahoots run:*)\"".into(),
                    ],
                },
                Paste {
                    heading: "Add to ~/second/default.rules:".into(),
                    lines: vec![long.clone()],
                },
            ],
        };
        assert_eq!(
            lines(&ending, Colors::ON)[3..],
            [
                String::new(),
                "Add to ~/first/settings.json (permissions.allow):".to_string(),
                "\"Bash(cahoots pick:*)\",".to_string(),
                "\"Bash(cahoots run:*)\"".to_string(),
                String::new(),
                "Add to ~/second/default.rules:".to_string(),
                long,
            ],
            "no rail, no color, no wrap"
        );
    }

    #[test]
    fn a_control_character_is_shown_escaped_and_never_sent() {
        let evil = "fine\x1b]0;owned\x07 then\x1b[2J\u{9b}31m";
        let ending = Ending {
            title: Some(evil.into()),
            blocks: vec![
                Block::done(evil, vec![evil.into()]),
                Block::Listing {
                    title: evil.into(),
                    items: vec![Item {
                        label: evil.into(),
                        value: evil.into(),
                        origin: Origin::Set,
                    }],
                },
                Block::Checks(vec![Checked {
                    mark: Mark::Failed,
                    label: evil.into(),
                    text: evil.into(),
                }]),
            ],
            last: Last::Refused(evil.into()),
            paste: vec![Paste {
                heading: evil.into(),
                lines: vec![evil.into()],
            }],
        };
        for colors in [Colors::OFF, Colors::ON] {
            let shown = lines(&ending, colors).join("\n");
            let raw = strip(&shown);
            assert!(!raw.contains('\x1b') && !raw.contains('\x07') && !raw.contains('\u{9b}'));
            assert!(
                raw.contains("fine\\u{1b}]0;owned\\u{7} then\\u{1b}[2J\\u{9b}31m"),
                "{raw}"
            );
        }
    }

    /// The text with the colors this module paints taken out.
    fn strip(line: &str) -> String {
        let mut out = String::new();
        let mut rest = line;
        while let Some(start) = rest.find("\x1b[") {
            out.push_str(&rest[..start]);
            let after = &rest[start..];
            rest = &after[after.find('m').map_or(after.len(), |end| end + 1)..];
        }
        out + rest
    }
}
