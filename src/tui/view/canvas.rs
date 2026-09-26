//! A grid of cells to draw a whole screen on: anywhere, and over what is
//! there. It turns into lines of text that change color only where the color
//! changes, and with colors off, into text with no escape codes at all.

use super::Colors;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Plain,
    Cyan,
    Green,
    Red,
    Gray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub color: Color,
    pub bold: bool,
    pub dim: bool,
}

pub const PLAIN: Style = Style {
    color: Color::Plain,
    bold: false,
    dim: false,
};

impl Style {
    pub const fn color(color: Color) -> Style {
        Style { color, ..PLAIN }
    }

    pub const fn dim() -> Style {
        Style { dim: true, ..PLAIN }
    }

    pub const fn bold() -> Style {
        Style {
            bold: true,
            ..PLAIN
        }
    }

    pub const fn bolded(self) -> Style {
        Style { bold: true, ..self }
    }

    fn sgr(self) -> String {
        let mut codes = vec!["0"];
        if self.bold {
            codes.push("1");
        }
        if self.dim {
            codes.push("2");
        }
        codes.push(match self.color {
            Color::Plain => "39",
            Color::Cyan => "36",
            Color::Green => "32",
            Color::Red => "31",
            Color::Gray => "90",
        });
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// Text in one style.
pub type Span = (String, Style);

pub fn span(text: impl Into<String>, style: Style) -> Span {
    (text.into(), style)
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    cells: Vec<(char, Style)>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas {
            w,
            h,
            cells: vec![(' ', PLAIN); w * h],
        }
    }

    /// Writes `text` from (row, col), cut at the right edge, and says the
    /// column after it.
    pub fn text(&mut self, row: usize, col: usize, text: &str, style: Style) -> usize {
        let mut at = col;
        if row >= self.h {
            return at;
        }
        for ch in text.chars() {
            if at >= self.w {
                break;
            }
            self.cells[row * self.w + at] = (ch, style);
            at += 1;
        }
        at
    }

    /// `text`, cut to `width` with an ellipsis when it does not fit.
    pub fn fit(&mut self, row: usize, col: usize, text: &str, width: usize, style: Style) -> usize {
        if text.chars().count() <= width {
            return self.text(row, col, text, style);
        }
        if width == 0 {
            return col;
        }
        let cut: String = text.chars().take(width - 1).collect();
        let at = self.text(row, col, &cut, style);
        self.text(row, at, "…", style)
    }

    /// `spans`, one after another, cut with an ellipsis `width` columns on.
    pub fn spans(&mut self, row: usize, col: usize, spans: &[Span], width: usize) -> usize {
        let mut at = col;
        for (text, style) in spans {
            let room = (col + width).saturating_sub(at);
            let long = text.chars().count() > room;
            at = self.fit(row, at, text, room, *style);
            if long {
                break;
            }
        }
        at
    }

    pub fn fill(&mut self, row: usize, col: usize, width: usize, ch: char, style: Style) {
        if row >= self.h {
            return;
        }
        for at in col..(col + width).min(self.w) {
            self.cells[row * self.w + at] = (ch, style);
        }
    }

    /// Everything drawn so far, faded: what a box is drawn over.
    pub fn fade(&mut self) {
        for cell in &mut self.cells {
            let color = match cell.1.color {
                Color::Plain => Color::Plain,
                _ => Color::Gray,
            };
            cell.1 = Style {
                color,
                bold: false,
                dim: true,
            };
        }
    }

    /// A box with rounded corners and `title` in its top edge, empty inside.
    pub fn frame(
        &mut self,
        top: usize,
        left: usize,
        size: (usize, usize),
        title: &str,
        edge: Style,
    ) {
        let (width, height) = size;
        if width < 2 || height < 2 {
            return;
        }
        let right = left + width - 1;
        let bottom = top + height - 1;
        for row in top..=bottom {
            self.fill(row, left, width, ' ', PLAIN);
        }
        self.fill(top, left + 1, width - 2, '─', edge);
        self.fill(bottom, left + 1, width - 2, '─', edge);
        for row in top + 1..bottom {
            self.text(row, left, "│", edge);
            self.text(row, right, "│", edge);
        }
        self.text(top, left, "╭", edge);
        self.text(top, right, "╮", edge);
        self.text(bottom, left, "╰", edge);
        self.text(bottom, right, "╯", edge);
        if !title.is_empty() {
            let at = self.text(top, left + 2, " ", edge);
            let at = self.fit(top, at, title, width.saturating_sub(6), Style::bold());
            self.text(top, at, " ", edge);
        }
    }

    /// Each row as a line of text, its trailing blanks left off.
    pub fn lines(&self, colors: Colors) -> Vec<String> {
        (0..self.h)
            .map(|row| {
                let cells = &self.cells[row * self.w..(row + 1) * self.w];
                let end = cells
                    .iter()
                    .rposition(|&(ch, _)| ch != ' ')
                    .map_or(0, |at| at + 1);
                let mut line = String::new();
                let mut current = PLAIN;
                for &(ch, style) in &cells[..end] {
                    if colors.0 && style != current {
                        line.push_str(&style.sgr());
                        current = style;
                    }
                    line.push(ch);
                }
                if current != PLAIN {
                    line.push_str("\x1b[0m");
                }
                line
            })
            .collect()
    }
}

/// The words of `text` in lines at most `width` wide.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_cut_at_the_edge_and_an_ellipsis_says_so() {
        let mut canvas = Canvas::new(10, 2);
        canvas.text(0, 7, "overflow", PLAIN);
        canvas.fit(1, 0, "a long label", 6, PLAIN);
        assert_eq!(canvas.lines(Colors::OFF), ["       ove", "a lon…"]);
    }

    #[test]
    fn color_changes_only_where_the_style_does_and_off_is_plain_text() {
        let mut canvas = Canvas::new(8, 1);
        canvas.text(0, 0, "ab", Style::color(Color::Cyan));
        canvas.text(0, 2, "cd", Style::color(Color::Cyan));
        canvas.text(0, 4, "ef", PLAIN);
        assert_eq!(
            canvas.lines(Colors::ON),
            ["\x1b[0;36mabcd\x1b[0;39mef"],
            "one code for a run of one style"
        );
        assert_eq!(canvas.lines(Colors::OFF), ["abcdef"]);
    }

    #[test]
    fn a_frame_is_a_rounded_box_with_its_title_in_the_top_edge() {
        let mut canvas = Canvas::new(12, 3);
        canvas.fill(1, 0, 12, 'x', PLAIN);
        canvas.frame(0, 1, (10, 3), "Cap", PLAIN);
        assert_eq!(
            canvas.lines(Colors::OFF),
            [" ╭─ Cap ──╮", "x│        │x", " ╰────────╯"]
        );
    }

    #[test]
    fn words_wrap_at_a_width() {
        assert_eq!(
            wrap("a run starts only while less is used", 12),
            ["a run starts", "only while", "less is used"]
        );
    }
}
