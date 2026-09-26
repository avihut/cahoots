//! A conversation on the rail: its intro, the questions, each drawn and then
//! redrawn in place as keys come, and its outro (or, when a question is left
//! unanswered, its cancel line).

use std::io::Write;

use super::Choice;
use super::keys::{Key, Keys};
use super::select::Select;
use super::view::{self, Colors};

pub struct Rail<'a, W: Write> {
    screen: &'a mut W,
    colors: Colors,
}

impl<'a, W: Write> Rail<'a, W> {
    pub fn new(screen: &'a mut W, colors: Colors) -> Self {
        Rail { screen, colors }
    }

    pub fn intro(&mut self, title: &str) {
        self.draw(&view::intro(title, self.colors), 0);
    }

    pub fn outro(&mut self, message: &str) {
        self.draw(&view::outro(message, self.colors), 0);
    }

    pub fn cancel(&mut self, message: &str) {
        self.draw(&view::cancel(message, self.colors), 0);
    }

    /// Asks `prompt` with `choices`: the index of the one chosen, or `None`
    /// when the question is left (Esc, Ctrl-C, Ctrl-D, or input ending).
    pub fn select(
        &mut self,
        prompt: &str,
        choices: &[Choice],
        keys: &mut impl Keys,
    ) -> Option<usize> {
        if choices.is_empty() {
            return None;
        }
        let mut state = Select::new();
        let mut drawn = 0;
        loop {
            drawn = self.draw(&view::select(prompt, choices, state, self.colors), drawn);
            if state.is_over() {
                return state.answer();
            }
            let pressed = keys.pressed();
            if pressed.is_empty() {
                state = state.press(Key::Cancel, choices.len());
            }
            for key in pressed {
                state = state.press(key, choices.len());
            }
        }
    }

    /// Draws `frame` over the last `drawn` lines (with none, below what is
    /// there), leaves the cursor at the start of the line after it, and says how
    /// many lines it took. With line wrap off (`Terminal`), that is one row each.
    fn draw(&mut self, frame: &[String], drawn: usize) -> usize {
        let mut text = String::new();
        if drawn > 0 {
            text.push_str(&format!("\x1b[{drawn}A\r\x1b[J"));
        }
        for line in frame {
            text.push_str(line);
            text.push('\n');
        }
        let _ = self.screen.write_all(text.as_bytes());
        let _ = self.screen.flush();
        frame.len()
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

    fn ask(keys: &[Key]) -> (Option<usize>, String) {
        let mut screen = Vec::new();
        let chosen = Rail::new(&mut screen, Colors::OFF).select(
            "Which?",
            &choices(),
            &mut keys.iter().copied(),
        );
        (chosen, String::from_utf8(screen).unwrap())
    }

    #[test]
    fn a_question_is_redrawn_in_place_until_it_is_answered() {
        let (chosen, shown) = ask(&[Key::Down, Key::Enter]);
        assert_eq!(chosen, Some(1));
        // Open on the first, open on the second, answered: each drawn over the
        // five lines of the one before.
        let frames: Vec<&str> = shown.split("\x1b[5A\r\x1b[J").collect();
        assert_eq!(
            frames,
            [
                "◆  Which?\n│  ● agent-usage (percentages)\n│  ○ ccusage\n│  ○ none\n└\n",
                "◆  Which?\n│  ○ agent-usage\n│  ● ccusage (token counts)\n│  ○ none\n└\n",
                "◇  Which?\n│  ccusage\n│\n",
            ]
        );
    }

    #[test]
    fn a_question_is_left_on_esc_or_when_the_keys_run_out() {
        let (chosen, shown) = ask(&[Key::Down, Key::Down, Key::Cancel]);
        assert_eq!(chosen, None);
        assert!(
            shown.ends_with("\x1b[5A\r\x1b[J■  Which?\n│  none\n│\n"),
            "{shown:?}"
        );
        assert_eq!(ask(&[]).0, None, "the input ended");
        assert_eq!(ask(&[Key::Other, Key::Enter]).0, Some(0));
    }

    #[test]
    fn a_conversation_opens_and_closes_around_its_questions() {
        let mut screen = Vec::new();
        let mut rail = Rail::new(&mut screen, Colors::OFF);
        rail.intro("cahoots install");
        assert_eq!(
            rail.select("Which?", &[], &mut [Key::Enter].into_iter()),
            None
        );
        rail.outro("Done");
        rail.cancel("Or not");
        assert_eq!(
            String::from_utf8(screen).unwrap(),
            "┌  cahoots install\n│\n└  Done\n└  Or not\n",
            "a question with no choices is not drawn"
        );
    }
}
