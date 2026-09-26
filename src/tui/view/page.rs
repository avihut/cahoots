//! How the settings page looks: one list of every setting under its
//! section's heading, `❯` on the highlighted one, the values in a column —
//! dim while they are defaults, marked `•` once set — and a box drawn over
//! the list, just under the setting it changes, to change it. The box speaks
//! the rail's shapes: `●`/`○` and `✓` for choices, `◀ ▶` for a number, `↕`
//! for an item being moved. `docs/TUI.md` has the vocabulary.

use super::super::page::{Edit, Editor, Origin, Page, Row};
use super::super::stepper::{Unit, number};
use super::Colors;
use super::canvas::{Canvas, Color, PLAIN, Span, Style, span, wrap};

/// The rows under the list: a blank, the help, the note, the status line.
const FOOTER: usize = 5;
/// The list starts under the title and its rule.
const TOP: usize = 2;
const MOVE: &str = "↑↓ move · tab section · enter change · esc close";

enum Line {
    Heading(usize),
    Blank,
    Row(usize),
}

/// The page on a screen `w` columns wide and `h` rows high, a line a row. It
/// scrolls the list to keep the highlighted row in view.
pub fn render(page: &mut Page, w: usize, h: usize, colors: Colors) -> Vec<String> {
    let mut c = Canvas::new(w, h);
    let gray = Style::color(Color::Gray);
    c.text(0, 1, "cahoots settings", Style::bold());
    let file = page.file.chars().count();
    if w > file + 20 {
        c.text(0, w - file - 1, &page.file, Style::dim());
    }
    c.fill(1, 0, w, '─', gray);
    if page.rows.is_empty() {
        return c.lines(colors);
    }

    let mut lines = Vec::new();
    for (n, row) in page.rows.iter().enumerate() {
        if n == 0 || page.rows[n - 1].section != row.section {
            if n > 0 {
                lines.push(Line::Blank);
            }
            lines.push(Line::Heading(n));
        }
        lines.push(Line::Row(n));
    }
    let label_width = page
        .rows
        .iter()
        .map(|row| row.label.chars().count())
        .max()
        .unwrap_or(0);
    let value_col = 3 + label_width + 4;
    let view = h.saturating_sub(TOP + FOOTER).max(1);
    let active = lines
        .iter()
        .position(|line| matches!(line, Line::Row(n) if *n == page.at))
        .unwrap_or(0);
    // A section's first row brings its heading into view with it.
    let heading = matches!(
        active.checked_sub(1).map(|n| &lines[n]),
        Some(Line::Heading(_))
    );
    follow(
        &mut page.scroll,
        (if heading { active - 1 } else { active }, active + 1),
        view,
        lines.len(),
    );
    for (n, line) in lines.iter().enumerate().skip(page.scroll).take(view) {
        let at = TOP + n - page.scroll;
        match line {
            Line::Blank => {}
            Line::Heading(first) => {
                c.text(at, 1, &page.rows[*first].section, Style::bold());
            }
            Line::Row(n) => row(&mut c, at, &page.rows[*n], *n == page.at, value_col),
        }
    }
    if page.scroll > 0 {
        c.text(TOP, w.saturating_sub(2), "↑", Style::dim());
    }
    if page.scroll + view < lines.len() {
        c.text(TOP + view - 1, w.saturating_sub(2), "↓", Style::dim());
    }

    let current = page.rows[page.at].clone();
    let width = w.saturating_sub(4);
    if page.editor.is_none() {
        let help = wrap(&current.help, width);
        for (n, line) in help.iter().take(2).enumerate() {
            c.text(h.saturating_sub(4) + n, 1, line, Style::dim());
        }
        c.fit(h.saturating_sub(2), 1, &current.note, width, Style::dim());
    }
    let status = match (&page.flash, &page.error, &page.editor) {
        (Some(flash), _, _) => span(flash.clone(), Style::color(Color::Green)),
        (_, Some(error), None) => span(error.clone(), Style::color(Color::Red)),
        _ => span(MOVE, Style::dim()),
    };
    c.spans(h.saturating_sub(1), 1, &[status], w.saturating_sub(2));

    if let Some(editor) = &page.editor {
        c.fade();
        let at = TOP + active - page.scroll;
        row(&mut c, at, &current, true, value_col);
        boxed(&mut c, at, &current, editor, page.error.as_deref());
    }
    c.lines(colors)
}

/// One setting's line in the list.
fn row(c: &mut Canvas, at: usize, row: &Row, active: bool, value_col: usize) {
    let cyan = Style::color(Color::Cyan);
    if active {
        c.text(at, 1, "❯", cyan);
    }
    c.text(at, 3, &row.label, if active { cyan } else { PLAIN });
    let style = match (active, row.origin) {
        (true, _) => cyan,
        (false, Origin::Set) => PLAIN,
        (false, _) => Style::dim(),
    };
    if row.origin == Origin::Set {
        c.text(at, value_col - 2, "•", style);
    }
    let room = c.w.saturating_sub(value_col + 3);
    c.fit(at, value_col, &row.value, room, style);
}

/// The box that changes `row`, under its line, or over it when there is no
/// room below.
fn boxed(c: &mut Canvas, at: usize, row: &Row, editor: &Editor, error: Option<&str>) {
    let width = c.w.saturating_sub(5).min(70);
    if width < 12 {
        return;
    }
    let mut inside: Vec<Vec<Span>> = wrap(&row.help, width - 4)
        .into_iter()
        .map(|line| vec![span(line, Style::dim())])
        .collect();
    inside.push(Vec::new());
    inside.extend(body(row, editor));
    if let Some(error) = error {
        inside.push(Vec::new());
        for line in wrap(error, width - 4) {
            inside.push(vec![span(line, Style::color(Color::Red))]);
        }
    }
    inside.push(Vec::new());
    inside.push(vec![span(hint(editor), Style::dim())]);
    let height = inside.len() + 2;
    let top = if at + height < c.h.saturating_sub(1) {
        at + 1
    } else {
        at.saturating_sub(height)
    };
    for line in top..top + height {
        c.fill(line, 0, 3, ' ', PLAIN);
    }
    c.frame(
        top,
        3,
        (width, height),
        &format!("{} · {}", row.section, row.label),
        Style::color(Color::Gray),
    );
    for (n, line) in inside.iter().enumerate() {
        c.spans(top + 1 + n, 5, line, width - 4);
    }
}

/// What the box holds for each way of changing a setting.
fn body(row: &Row, editor: &Editor) -> Vec<Vec<Span>> {
    let green = Style::color(Color::Green);
    let cyan = Style::color(Color::Cyan);
    match (editor, &row.edit) {
        (Editor::Choose(select), Edit::Choose { choices, current }) => {
            let highlighted = select.highlighted();
            choices
                .iter()
                .enumerate()
                .map(|(n, choice)| {
                    let mut line = if n == highlighted {
                        vec![span("● ", green), span(choice.label.clone(), PLAIN)]
                    } else {
                        vec![
                            span("○ ", Style::dim()),
                            span(choice.label.clone(), Style::dim()),
                        ]
                    };
                    if Some(n) == *current {
                        line.push(span(" ✓", Style::color(Color::Gray)));
                    }
                    if n == highlighted && !choice.hint.is_empty() {
                        line.push(span(format!(" ({})", choice.hint), Style::dim()));
                    }
                    line
                })
                .collect()
        }
        (Editor::Step(stepper), _) => {
            let shown = stepper
                .value
                .map_or("not set".to_string(), |n| number(n, stepper.unit));
            let mut lines = vec![vec![
                span("◀  ", cyan),
                span(shown, Style::bold()),
                span("  ▶", cyan),
            ]];
            if stepper.unit == Unit::Percent {
                let filled = stepper.value.unwrap_or(0).clamp(0, 100) as usize * 30 / 100;
                lines.push(vec![
                    span("━".repeat(filled), cyan),
                    span("─".repeat(30 - filled), Style::dim()),
                ]);
            }
            let default = stepper
                .default
                .map_or("not set".to_string(), |n| number(n, stepper.unit));
            lines.push(vec![span(format!("default {default}"), Style::dim())]);
            lines
        }
        (Editor::Order(order), Edit::Order { items }) => order
            .items
            .iter()
            .enumerate()
            .map(|(n, item)| {
                let place = format!("{}. ", n + 1);
                let item = items[*item].clone();
                match (n == order.at, order.held) {
                    (true, true) => vec![
                        span("↕ ", cyan.bolded()),
                        span(place, cyan),
                        span(item, cyan),
                    ],
                    (true, false) => vec![span("● ", green), span(place, PLAIN), span(item, PLAIN)],
                    _ => vec![
                        span("○ ", Style::dim()),
                        span(place, Style::dim()),
                        span(item, Style::dim()),
                    ],
                }
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn hint(editor: &Editor) -> &'static str {
    match editor {
        Editor::Choose(_) => "↑↓ choose · enter save · esc cancel",
        Editor::Step(_) => "←→ change · r default · enter save · esc cancel",
        Editor::Order(order) if order.held => {
            "↑↓ move it · space put down · enter save · esc cancel"
        }
        Editor::Order(_) => "↑↓ move · space pick up · enter save · esc cancel",
    }
}

/// Scrolls so lines `wanted.0..wanted.1` are in a view of `view` lines out
/// of `total` — or, when they do not all fit, the last of them.
fn follow(scroll: &mut usize, wanted: (usize, usize), view: usize, total: usize) {
    let (start, end) = wanted;
    if start < *scroll {
        *scroll = start;
    }
    if end > *scroll + view {
        *scroll = end.saturating_sub(view);
    }
    *scroll = (*scroll).min(total.saturating_sub(view));
}

#[cfg(test)]
mod tests {
    use super::super::super::Choice;
    use super::super::super::keys::Key;
    use super::*;

    fn row(id: &str, section: &str, value: &str, origin: Origin, edit: Edit) -> Row {
        Row {
            id: id.to_string(),
            section: section.to_string(),
            label: id.to_string(),
            value: value.to_string(),
            origin,
            help: format!("What {id} does."),
            note: format!("Where {id} comes from."),
            edit,
        }
    }

    fn page() -> Page {
        Page::new(
            "~/.config/cahoots/config.toml",
            vec![
                row("Enabled", "Codex", "on", Origin::Set, Edit::Toggle),
                row(
                    "Usage cap",
                    "Codex",
                    "75%",
                    Origin::Default,
                    Edit::Step {
                        value: Some(75),
                        min: 1,
                        max: 100,
                        unit: Unit::Percent,
                        default: Some(75),
                        start: 75,
                    },
                ),
                row(
                    "Meter",
                    "Usage meter",
                    "ccusage",
                    Origin::Found,
                    Edit::Choose {
                        choices: vec![
                            Choice::new("agent-usage", "percentages"),
                            Choice::new("ccusage", "token counts"),
                        ],
                        current: Some(1),
                    },
                ),
            ],
        )
    }

    fn shown(page: &mut Page, w: usize, h: usize) -> Vec<String> {
        render(page, w, h, Colors::OFF)
    }

    #[test]
    fn the_list_shows_sections_values_and_where_each_comes_from() {
        let mut page = page();
        let screen = shown(&mut page, 60, 14);
        assert_eq!(
            screen[..8],
            [
                " cahoots settings             ~/.config/cahoots/config.toml",
                "────────────────────────────────────────────────────────────",
                " Codex",
                " ❯ Enabled    • on",
                "   Usage cap    75%",
                "",
                " Usage meter",
                "   Meter        ccusage",
            ]
        );
        assert_eq!(screen[10], " What Enabled does.");
        assert_eq!(screen[12], " Where Enabled comes from.");
        assert_eq!(screen[13], format!(" {MOVE}"));
    }

    #[test]
    fn a_box_opens_under_the_setting_and_says_what_its_keys_do() {
        let mut page = page();
        page.press(Key::Down);
        page.press(Key::Enter);
        page.press(Key::Left);
        let screen = shown(&mut page, 60, 20).join("\n");
        for part in [
            " ❯ Usage cap    75%",
            "   ╭─ Codex · Usage cap ─",
            "│ ◀  70%  ▶",
            "│ ━━━━━━━━━━━━━━━━━━━━━─────────",
            "│ default 75%",
            "│ ←→ change · r default · enter save · esc cancel",
        ] {
            assert!(screen.contains(part), "{part:?} in\n{screen}");
        }
    }

    #[test]
    fn a_choice_marks_the_highlighted_one_and_what_is_chosen_now() {
        let mut page = page();
        page.press(Key::Tab);
        page.press(Key::Enter);
        page.press(Key::Up);
        let screen = shown(&mut page, 60, 20).join("\n");
        assert!(
            screen.contains("│ ● agent-usage (percentages)") && screen.contains("│ ○ ccusage ✓"),
            "{screen}"
        );
        page.refused("Must be above the usage cap, 75%.");
        let screen = shown(&mut page, 60, 20).join("\n");
        assert!(
            screen.contains("│ Must be above the usage cap, 75%."),
            "{screen}"
        );
    }

    #[test]
    fn a_long_list_scrolls_to_keep_the_highlight_in_view() {
        let mut page = page();
        page.press(Key::Tab);
        let screen = shown(&mut page, 60, 8);
        assert!(
            screen.iter().any(|line| line.contains("❯ Meter")),
            "{screen:#?}"
        );
        assert!(screen[2].ends_with('↑'), "{screen:#?}");
    }

    #[test]
    fn with_colors_on_the_shapes_and_words_are_the_same() {
        let mut page = page();
        let plain = shown(&mut page, 60, 14);
        let colored: Vec<String> = render(&mut page, 60, 14, Colors::ON)
            .iter()
            .map(|line| strip(line))
            .collect();
        assert_eq!(colored, plain);
    }

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
