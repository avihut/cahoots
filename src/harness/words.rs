//! The callee's words: what a run said along the way, held to a bound before
//! an agent verb shows it. Its latest activity, its notes and its account of
//! a failure all come out of a callee's output, which a prompt-injected or
//! simply chatty callee writes. So each is one line of plain text, cut to a
//! length, with nothing in it that could move a cursor, hide a line or turn
//! the text around, and it is shown marked `untrusted`
//! (docs/THREAT-MODEL.md, "A run's words are the callee's").

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The longest latest activity: a line, not a paragraph.
pub const ACTIVITY_CHARS: usize = 200;
/// The longest note: a warning the callee's stream printed.
pub const NOTE_CHARS: usize = 300;
/// The longest account of a failure: the stream's reason or stderr's end.
pub const FAILURE_CHARS: usize = 500;
/// The most notes a run keeps. A retry notice can repeat for as long as a
/// run lasts; the first ones say what happened.
pub const MAX_NOTES: usize = 20;

/// Which part of a text outlives the bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    /// The first line that says anything, from its start: a progress line.
    FirstLine,
    /// All of it as one line, from its start: a stream's reason.
    Start,
    /// All of it as one line, its end: stderr, where an error comes last.
    End,
}

/// Text from a callee, bounded. Built only by [`CalleeText::bound`], and
/// bounded again whenever it is shown, so a record edited on disk is held to
/// the same bound as one the supervisor wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "OnDisk")]
pub struct CalleeText {
    text: String,
    truncated: bool,
}

/// What a record may hold: bounded text, or a plain string from a record
/// written before notes were bounded. Either is bounded again when shown.
#[derive(Deserialize)]
#[serde(untagged)]
enum OnDisk {
    Bounded { text: String, truncated: bool },
    Plain(String),
}

impl From<OnDisk> for CalleeText {
    fn from(on_disk: OnDisk) -> CalleeText {
        match on_disk {
            OnDisk::Bounded { text, truncated } => CalleeText { text, truncated },
            OnDisk::Plain(text) => CalleeText {
                text,
                truncated: false,
            },
        }
    }
}

impl CalleeText {
    /// `None` when nothing is left to say once the bound is applied.
    pub fn bound(text: &str, cap: usize, keep: Keep) -> Option<CalleeText> {
        let mut lines = text
            .split(is_line_break)
            .map(|line| {
                line.chars()
                    .map(|c| if is_hidden(c) { ' ' } else { c })
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .filter(|line| !line.is_empty());
        let (line, mut truncated) = match keep {
            Keep::FirstLine => {
                let first = lines.next()?;
                (first, lines.next().is_some())
            }
            Keep::Start | Keep::End => (lines.collect::<Vec<_>>().join(" "), false),
        };
        if line.is_empty() {
            return None;
        }
        let count = line.chars().count();
        let text = if count > cap {
            truncated = true;
            match keep {
                Keep::End => line.chars().skip(count - cap).collect(),
                Keep::FirstLine | Keep::Start => line.chars().take(cap).collect(),
            }
        } else {
            line
        };
        Some(CalleeText { text, truncated })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// As an agent verb shows it: bounded again, and marked as the callee's.
    pub fn shown(&self, cap: usize) -> Value {
        let again = CalleeText::bound(&self.text, cap, Keep::Start);
        json!({
            "untrusted": true,
            "text": again.as_ref().map_or("", |t| t.text.as_str()),
            "truncated": self.truncated || again.is_none_or(|t| t.truncated),
        })
    }
}

/// A line ends here, whichever convention the callee used.
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

/// A character that is not text a reader sees, and that a model may read
/// all the same: a control character (ESC and the rest of C0 and C1); every
/// format character (Unicode's `Cf`) — a zero-width character, a bidi
/// control, a soft hyphen, a BOM, and the tag characters, invisible ASCII
/// that can spell out an instruction; a variation selector, which can carry
/// bytes the same way; and the fillers that show as nothing. The list is
/// written out because no Unicode table is a dependency (AGENTS.md, rule 9).
fn is_hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            // Format characters (Cf).
            '\u{AD}'
                | '\u{600}'..='\u{605}'
                | '\u{61C}'
                | '\u{6DD}'
                | '\u{70F}'
                | '\u{890}'..='\u{891}'
                | '\u{8E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                // Tags: language tags, and invisible ASCII.
                | '\u{E0000}'..='\u{E007F}'
                // Variation selectors, and the grapheme joiner.
                | '\u{34F}'
                | '\u{180B}'..='\u{180F}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{E0100}'..='\u{E01EF}'
                // Fillers and blanks that show as nothing.
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{2800}'
                | '\u{3164}'
                | '\u{FFA0}'
        )
}

/// What kind of step a run took last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    /// It said something: a progress line, or the start of its answer.
    Said,
    /// It called a tool.
    Tool,
}

/// cahoots' own name for a tool step — never the callee's name for its tool,
/// which is the callee's to choose. A closed set: growing it is a code change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolLabel {
    Read,
    Search,
    Edit,
    Command,
    Web,
    Mcp,
    Other,
}

/// A run's latest activity: what it said last, or the tool it called last
/// and on what. Only ever from the callee's own text and its own tool calls,
/// never from a tool's result — that is the repository's text, or the web's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    kind: ActivityKind,
    tool: Option<ToolLabel>,
    /// What it said, or what the tool was called on. Empty for a tool step
    /// with nothing to say it was called on.
    text: Option<CalleeText>,
}

impl Activity {
    pub fn said(text: &str) -> Option<Activity> {
        Some(Activity {
            kind: ActivityKind::Said,
            tool: None,
            text: Some(CalleeText::bound(text, ACTIVITY_CHARS, Keep::FirstLine)?),
        })
    }

    pub fn tool(label: ToolLabel, on: Option<&str>) -> Activity {
        Activity {
            kind: ActivityKind::Tool,
            tool: Some(label),
            text: on.and_then(|on| CalleeText::bound(on, ACTIVITY_CHARS, Keep::FirstLine)),
        }
    }

    /// As `status` shows it, with the moment the supervisor saw it.
    pub fn shown(&self, at: Option<u64>) -> Value {
        let text = self.text.as_ref().map(|text| text.shown(ACTIVITY_CHARS));
        json!({
            "untrusted": true,
            "kind": self.kind,
            "tool": self.tool,
            "text": text.as_ref().map_or(json!(""), |t| t["text"].clone()),
            "truncated": text.is_some_and(|t| t["truncated"] == true),
            "at": at,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_line(text: &str) -> Option<CalleeText> {
        CalleeText::bound(text, ACTIVITY_CHARS, Keep::FirstLine)
    }

    #[test]
    fn a_short_line_is_kept_as_it_is() {
        let said = first_line("Reading the gate.").unwrap();
        assert_eq!(said.text(), "Reading the gate.");
        assert!(!said.truncated());
    }

    #[test]
    fn a_long_line_is_cut_on_a_character_and_says_so() {
        let said = first_line(&"é".repeat(1000)).unwrap();
        assert_eq!(said.text().chars().count(), ACTIVITY_CHARS);
        assert!(said.truncated());
        let tail = CalleeText::bound(&format!("{}END", "x".repeat(600)), 10, Keep::End).unwrap();
        assert_eq!(tail.text(), "xxxxxxxEND");
        assert!(tail.truncated());
    }

    #[test]
    fn nothing_hidden_survives() {
        for hostile in [
            "a\u{1b}[2Jb",
            "a\u{1b}]0;title\u{07}b",
            "a\u{7}b",
            "a\u{202E}b",
            "a\u{200B}b",
            "a\u{2066}b",
            "a\u{FEFF}b",
            "a\u{9b}31mb",
            "a\tb",
            "a\u{E0049}\u{E0047}\u{E004E}b",
            "a\u{FE0F}\u{E0100}b",
            "a\u{2064}\u{206A}b",
            "a\u{3164}\u{2800}b",
            "a\u{1D173}b",
        ] {
            let said = CalleeText::bound(hostile, 50, Keep::Start).unwrap();
            assert!(
                !said.text().chars().any(is_hidden),
                "{hostile:?} → {:?}",
                said.text()
            );
            assert!(said.text().starts_with('a') && said.text().ends_with('b'));
        }
        assert_eq!(
            CalleeText::bound("plain words, kept", 50, Keep::Start)
                .unwrap()
                .text(),
            "plain words, kept"
        );
    }

    #[test]
    fn a_progress_line_is_the_first_line_that_says_anything() {
        for text in [
            "\n  \nfirst\nsecond",
            "first\u{2028}second",
            "first\u{2029}second",
            "first\r\nsecond",
        ] {
            let said = first_line(text).unwrap();
            assert_eq!(said.text(), "first", "{text:?}");
            assert!(said.truncated(), "{text:?}");
        }
        assert!(first_line(" \n\t\u{200B}\n").is_none());
        assert!(first_line("").is_none());
        // Kept whole, a text's lines become one.
        let joined = CalleeText::bound("first\nsecond", 50, Keep::Start).unwrap();
        assert_eq!(joined.text(), "first second");
        assert!(!joined.truncated());
    }

    #[test]
    fn what_a_record_holds_is_bounded_again_when_shown() {
        let on_disk: CalleeText = serde_json::from_value(json!({
            "text": format!("{}\u{1b}[31m\nhidden", "y".repeat(5000)),
            "truncated": false,
        }))
        .unwrap();
        let shown = on_disk.shown(ACTIVITY_CHARS);
        let text = shown["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), ACTIVITY_CHARS);
        assert!(!text.contains('\u{1b}') && !text.contains('\n'));
        assert_eq!(shown["truncated"], true);
        assert_eq!(shown["untrusted"], true);

        // A note from before notes were bounded was a plain string.
        let old: CalleeText = serde_json::from_value(json!("an old note\u{1b}[2J")).unwrap();
        assert_eq!(
            old.shown(NOTE_CHARS),
            json!({"untrusted": true, "text": "an old note [2J", "truncated": false})
        );

        let fine = first_line("fine").unwrap().shown(ACTIVITY_CHARS);
        assert_eq!(
            fine,
            json!({"untrusted": true, "text": "fine", "truncated": false})
        );
    }

    #[test]
    fn an_activity_is_shown_marked_as_the_callees() {
        let step = Activity::tool(ToolLabel::Read, Some("src/gate.rs"));
        assert_eq!(
            step.shown(Some(7)),
            json!({"untrusted": true, "kind": "tool", "tool": "read",
                   "text": "src/gate.rs", "truncated": false, "at": 7})
        );
        let bare = Activity::tool(ToolLabel::Mcp, None).shown(None);
        assert_eq!(bare["text"], "");
        assert_eq!(bare["truncated"], false);
        assert!(Activity::said("\n\n").is_none());
    }
}
