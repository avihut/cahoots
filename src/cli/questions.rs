//! The questions cahoots asks a person: the words of each one, and what each
//! answer means. This is where the logic and the interface meet (hard rule
//! 11). The logic hands a question over as data (`Decision::Ask`) and takes the
//! answer back as data (`detect::picked`), and the TUI draws what it is given
//! and says which choice was made (`docs/TUI.md`). Nothing here decides
//! anything.

use std::io::Write;

use crate::exit::{Exit, Fail, Res};
use crate::meter::Selection;
use crate::meter::detect::Found;
use crate::tui::{Choice, Colors, Keys, Rail};

pub(super) const NO_METER: &str = "only cahoots' own runs-per-hour limit";

/// `install`'s question: which of the usage meters found to use, or none.
/// Asked before `install` writes anything, so leaving it writes nothing.
pub fn which_meter(
    options: &[Found],
    keys: &mut impl Keys,
    screen: &mut impl Write,
    colors: Colors,
) -> Res<Selection> {
    let mut choices: Vec<Choice> = options
        .iter()
        .map(|found| Choice::new(found.meter.as_str(), found.meter.summary()))
        .collect();
    choices.push(Choice::new("none", NO_METER));
    let mut rail = Rail::new(screen, colors);
    rail.intro("cahoots install");
    let Some(chosen) = rail.select("Which usage meter should cahoots use?", &choices, keys) else {
        rail.cancel("No meter chosen, and nothing written");
        return Err(meter_unanswered());
    };
    Ok(match options.get(chosen) {
        Some(found) => {
            rail.outro(&format!("Usage meter: {}", found.meter));
            Selection::Meter(found.meter)
        }
        None => {
            rail.outro(&format!("No usage meter: {NO_METER}"));
            Selection::NoMeter
        }
    })
}

const METER_FLAG: &str =
    "`cahoots install --meter <agent-usage|ccusage|none>` chooses without asking";

fn meter_unanswered() -> Fail {
    Fail::new(Exit::Usage, format!("no meter chosen — {METER_FLAG}"))
}

/// There is no terminal to ask the meter question at.
pub fn no_terminal_for_meter() -> Fail {
    Fail::new(
        Exit::Usage,
        format!(
            "more than one usage meter was found, and which to use is asked at a terminal \
             (stdin and stderr, `TERM` not `dumb`) — {METER_FLAG}"
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::meter::MeterId;
    use crate::tui::Key;

    fn found(meter: MeterId, binary: &str) -> Found {
        Found {
            meter,
            binary: PathBuf::from(binary),
            version: None,
            note: None,
            unusable: None,
        }
    }

    fn ask(keys: &[Key]) -> (Res<Selection>, String) {
        let options = [
            found(
                MeterId::AgentUsage,
                "/Applications/AgentUsage.app/Contents/MacOS/usage-cli",
            ),
            found(MeterId::Ccusage, "/opt/homebrew/bin/ccusage"),
        ];
        let mut screen = Vec::new();
        let answer = which_meter(
            &options,
            &mut keys.iter().copied(),
            &mut screen,
            Colors::OFF,
        );
        (answer, String::from_utf8(screen).unwrap())
    }

    #[test]
    fn install_asks_which_meter_on_the_rail_and_enter_takes_the_first() {
        let (answer, shown) = ask(&[Key::Enter]);
        assert_eq!(answer.unwrap(), Selection::Meter(MeterId::AgentUsage));
        assert!(
            shown.starts_with(
                "┌  cahoots install\n│\n\
                 ◆  Which usage meter should cahoots use?\n\
                 │  ● agent-usage (each plan's own percentages, as the vendors report them)\n\
                 │  ○ ccusage\n\
                 │  ○ none\n\
                 └\n"
            ),
            "{shown:?}"
        );
        assert!(
            shown.ends_with(
                "◇  Which usage meter should cahoots use?\n│  agent-usage\n│\n\
                 └  Usage meter: agent-usage\n"
            ),
            "{shown:?}"
        );
    }

    #[test]
    fn each_choice_is_an_answer_the_logic_understands() {
        assert_eq!(
            ask(&[Key::Down, Key::Enter]).0.unwrap(),
            Selection::Meter(MeterId::Ccusage)
        );
        let (none, shown) = ask(&[Key::Up, Key::Enter]);
        assert_eq!(
            none.unwrap(),
            Selection::NoMeter,
            "up from the first is the last"
        );
        assert!(
            shown.ends_with("└  No usage meter: only cahoots' own runs-per-hour limit\n"),
            "{shown:?}"
        );
    }

    #[test]
    fn leaving_the_question_is_no_answer_and_says_which_flag_gives_one() {
        let (left, shown) = ask(&[Key::Down, Key::Cancel]);
        let fail = left.unwrap_err();
        assert_eq!(fail.exit, Exit::Usage);
        assert!(fail.message.contains("--meter"), "{}", fail.message);
        assert!(
            shown.ends_with(
                "■  Which usage meter should cahoots use?\n│  ccusage\n│\n\
                 └  No meter chosen, and nothing written\n"
            ),
            "{shown:?}"
        );
        assert_eq!(
            ask(&[]).0.unwrap_err().exit,
            Exit::Usage,
            "the keys ran out: unanswered"
        );
    }
}
