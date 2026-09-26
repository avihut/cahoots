//! Edits `config.toml` in place. A setting changes or goes, and the rest of
//! the file stays as its person wrote it: their comments, their order, the
//! shape of their tables. `toml_edit` holds the document, and
//! `UserConfig::parse` holds the result to everything a config may say before
//! a byte is written, so an edit can never widen what cahoots may do any more
//! than a hand edit can (hard rule 6).
//!
//! A person may edit the file by hand at any moment, so each change reads it
//! afresh. A file that does not parse is refused and left alone: its person
//! may be halfway through an edit.

use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use toml_edit::{DocumentMut, InlineTable, Item, KeyMut, RawString, Table, TableLike, Value};

use super::{SCHEMA, UserConfig};
use crate::exit::{Fail, Res};

/// Where a key sits: the tables it is in, then its own name, written the way
/// a dotted key is (`harness.codex.cap`). Every key cahoots edits is a bare
/// key, so a `.` always stands between two of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPath(Vec<String>);

impl KeyPath {
    pub fn of(dotted: &str) -> KeyPath {
        KeyPath(dotted.split('.').map(str::to_string).collect())
    }
}

impl fmt::Display for KeyPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.join("."))
    }
}

#[derive(Debug, Clone)]
pub enum Change {
    /// The key takes this value. A key already there keeps its place and the
    /// comment beside it; a new one joins its table, which is made if it is
    /// missing.
    Set { path: KeyPath, value: Value },
    /// The key goes, and its default applies again. So does a table: `Remove`
    /// takes whatever the path names. What was written above and beside it is
    /// kept, above whatever follows it.
    Remove { path: KeyPath },
}

/// Makes `changes` to the file (a missing one starts as `schema = 1`) and
/// returns the config it now holds. Nothing is written unless the result is a
/// config cahoots accepts, and nothing at all when the changes change nothing.
pub fn apply(file: &Path, changes: &[Change]) -> Res<UserConfig> {
    let (text, existed) = match fs::read_to_string(file) {
        Ok(text) => (text, true),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            (format!("schema = {SCHEMA}\n"), false)
        }
        Err(error) => {
            return Err(Fail::config(format!(
                "cannot read {}: {error}",
                file.display()
            )));
        }
    };
    let mut doc: DocumentMut = text.parse().map_err(|error: toml_edit::TomlError| {
        let line = error
            .span()
            .map_or(1, |span| text[..span.start].matches('\n').count() + 1);
        Fail::config(format!(
            "{} does not parse at line {line} ({}), so nothing was changed",
            file.display(),
            error.message().trim_end()
        ))
    })?;
    for change in changes {
        match change {
            Change::Set { path, value } => set(&mut doc, path, value.clone())?,
            Change::Remove { path } => remove(&mut doc, &path.0),
        }
    }
    let edited = doc.to_string();
    let config = UserConfig::parse(&edited)?;
    if !existed || edited != text {
        if let Some(dir) = file.parent() {
            crate::dirs::ensure_private_dir(dir)?;
        }
        crate::run::record::write_private_atomic(file, edited.as_bytes())?;
    }
    Ok(config)
}

/// How a table is written in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Under a `[header]` of its own, or implied by one below it
    /// (`[harness.codex]` implies `harness`). The top of the file is one too.
    Header,
    /// Dotted keys among another table's lines: `harness.codex.cap = 60`.
    Dotted,
    /// `{ cap = 60 }`.
    Inline,
}

impl Shape {
    fn of(item: &Item) -> Option<Shape> {
        match item {
            Item::Table(table) if table.is_dotted() => Some(Shape::Dotted),
            Item::Table(_) => Some(Shape::Header),
            Item::Value(Value::InlineTable(_)) => Some(Shape::Inline),
            _ => None,
        }
    }

    /// A new table inside a table of this shape, written the way its parent
    /// is. One that only leads to another is implicit: no empty `[harness]`.
    fn new_table(self, implicit: bool) -> Item {
        match self {
            Shape::Header => {
                let mut table = Table::new();
                table.set_implicit(implicit);
                Item::Table(table)
            }
            Shape::Dotted => {
                let mut table = Table::new();
                table.set_implicit(true);
                table.set_dotted(true);
                Item::Table(table)
            }
            Shape::Inline => Item::Value(Value::InlineTable(InlineTable::new())),
        }
    }
}

fn set(doc: &mut DocumentMut, path: &KeyPath, mut value: Value) -> Res<()> {
    let Some((key, tables)) = path.0.split_last() else {
        return Ok(());
    };
    let mut here: &mut dyn TableLike = doc.as_table_mut();
    let mut shape = Shape::Header;
    for (at, name) in tables.iter().enumerate() {
        if here.get(name).is_none_or(Item::is_none) {
            here.insert(name, shape.new_table(at + 1 < tables.len()));
        }
        let item = here.get_mut(name).expect("the table is there");
        shape = Shape::of(item)
            .ok_or_else(|| Fail::config(format!("{path}: `{name}` is not a table in the file")))?;
        here = item.as_table_like_mut().expect("a table");
    }
    match here.get_mut(key) {
        Some(Item::Value(old)) => {
            *value.decor_mut() = old.decor().clone();
            *old = value;
        }
        Some(Item::None) | None => {
            // In `{ … }`, the space before the `}` is the last value's, and a
            // value added at the end takes it on.
            if shape == Shape::Inline
                && let Some((_, Item::Value(last))) = here.iter_mut().last()
            {
                let closing = raw(last.decor().suffix(), "").to_string();
                last.decor_mut().set_suffix("");
                value.decor_mut().set_suffix(closing);
            }
            here.insert(key, Item::Value(value));
        }
        Some(_) => {
            return Err(Fail::config(format!(
                "{path} is a table in the file, not a value"
            )));
        }
    }
    Ok(())
}

/// Takes out whatever `path` names, keeps the comments that were written
/// above it and beside it, and then takes out a table that is left with
/// nothing in it and no comment of its own.
fn remove(doc: &mut DocumentMut, path: &[String]) {
    let Some((name, parent)) = path.split_last() else {
        return;
    };
    let Some(item) = item_at(doc.as_table(), path) else {
        return;
    };
    // Inside `{ … }` there is nothing written around a key to keep.
    let in_lines = parent.is_empty()
        || item_at(doc.as_table(), parent).and_then(Shape::of) != Some(Shape::Inline);
    let kept = in_lines.then(|| {
        (
            said(doc.as_table(), path, item),
            after(doc.as_table(), path, item),
        )
    });
    if let Some(table) = table_at_mut(doc.as_table_mut(), parent) {
        if !in_lines {
            hand_on_closing_space(table, name);
        }
        table.remove(name);
    }
    if let Some((said, after)) = kept {
        put(doc, &after, &said);
    }
    if !parent.is_empty() && item_at(doc.as_table(), parent).is_some_and(goes_when_empty) {
        remove(doc, parent);
    }
}

/// In `{ … }`, the space before the `}` is the last value's: when `name` is
/// the last and goes, the value before it takes the space on.
fn hand_on_closing_space(table: &mut dyn TableLike, name: &str) {
    let mut values: Vec<(KeyMut<'_>, &mut Value)> = table
        .iter_mut()
        .filter_map(|(key, item)| Some((key, item.as_value_mut()?)))
        .collect();
    if let [.., (_, before), (last, removed)] = values.as_mut_slice()
        && last.get() == name
    {
        before
            .decor_mut()
            .set_suffix(raw(removed.decor().suffix(), "").to_string());
    }
}

/// A table with nothing left in it and no comment of its own to keep it. An
/// empty `[header]` a person wrote a comment for stays, and so does the
/// comment.
fn goes_when_empty(item: &Item) -> bool {
    match item {
        Item::Table(table) => {
            table.is_empty()
                && (table.is_dotted()
                    || table.is_implicit()
                    || !(has_comment(raw(table.decor().prefix(), ""))
                        || has_comment(raw(table.decor().suffix(), ""))))
        }
        Item::Value(Value::InlineTable(table)) => table.is_empty(),
        _ => false,
    }
}

/// What was written around lines that are being removed: the text before the
/// first one (blank lines and comments), and every other comment on them.
#[derive(Debug, Default)]
struct Said {
    lead: String,
    comments: Vec<String>,
}

/// One line as the file prints it: what comes before it, and what comes
/// after it on the same line.
type Line = (String, String);

fn said(root: &Table, path: &[String], item: &Item) -> Said {
    let mut lines = Vec::new();
    match item {
        Item::Table(table) if !table.is_dotted() => header_lines(table, &mut lines),
        Item::Table(table) => body_lines(table, &mut lines),
        _ => {
            let (name, parent) = path.split_last().expect("a path names a key");
            let lead = table_at(root, parent)
                .and_then(|table| table.key(name))
                .map_or(String::new(), |key| {
                    raw(key.leaf_decor().prefix(), "").to_string()
                });
            let beside = item.as_value().map_or(String::new(), |value| {
                raw(value.decor().suffix(), "").to_string()
            });
            lines.push((lead, beside));
        }
    }
    let mut said = Said::default();
    for (at, (before, beside)) in lines.into_iter().enumerate() {
        if at == 0 {
            said.lead = before;
        } else {
            said.comments.extend(comment_lines(&before));
        }
        said.comments.extend(comment_lines(&beside));
    }
    said
}

/// A `[header]` table's lines: its header (when it prints one), its own lines,
/// then the tables under it.
fn header_lines(table: &Table, lines: &mut Vec<Line>) {
    if prints_header(table) {
        lines.push((
            raw(table.decor().prefix(), "\n").to_string(),
            raw(table.decor().suffix(), "").to_string(),
        ));
    }
    body_lines(table, lines);
    for (_, item) in table.iter() {
        if let Item::Table(child) = item
            && !child.is_dotted()
        {
            header_lines(child, lines);
        }
    }
}

/// Whether a `[header]` table prints its header: an implicit one only once it
/// has a line of its own.
fn prints_header(table: &Table) -> bool {
    !table.is_implicit() || !table.get_values().is_empty()
}

/// The `key = value` lines a table prints under its header, dotted keys
/// included.
fn body_lines(table: &Table, lines: &mut Vec<Line>) {
    for (keys, value) in table.get_values() {
        let key = keys.last().expect("a line has a key");
        lines.push((
            raw(key.leaf_decor().prefix(), "").to_string(),
            raw(value.decor().suffix(), "").to_string(),
        ));
    }
}

/// What the file prints right after the lines being removed.
#[derive(Debug, PartialEq)]
enum After {
    /// A `key = value` line, by its path.
    Key(Vec<String>),
    /// A `[header]`, by its table's path.
    Header(Vec<String>),
    /// Nothing: the end of the file.
    End,
}

fn after(root: &Table, path: &[String], item: &Item) -> After {
    if matches!(item, Item::Table(table) if !table.is_dotted()) {
        return next_header(root, path, true);
    }
    // The table whose lines hold this one: the nearest with a header (or the
    // top of the file).
    let mut owner = path.len() - 1;
    while owner > 0
        && !matches!(item_at(root, &path[..owner]), Some(Item::Table(table)) if !table.is_dotted())
    {
        owner -= 1;
    }
    let (body, inner) = path.split_at(owner);
    let Some(table) = table_at(root, body).and_then(|table| table.as_table_like()) else {
        return After::End;
    };
    let mut past = false;
    for (keys, _) in table.get_values() {
        let keys: Vec<&str> = keys.iter().map(|key| key.get()).collect();
        if keys.starts_with(&inner.iter().map(String::as_str).collect::<Vec<_>>()) {
            past = true;
        } else if past {
            return After::Key(
                body.iter()
                    .cloned()
                    .chain(keys.into_iter().map(str::to_string))
                    .collect(),
            );
        }
    }
    next_header(root, body, false)
}

/// The first `[header]` the file prints after the table at `path`'s own
/// header, leaving out the tables inside it when they are going with it.
fn next_header(root: &Table, path: &[String], skip_inside: bool) -> After {
    let order = headers(root);
    let Some(at) = order.iter().position(|(table, _)| table.as_slice() == path) else {
        return After::End;
    };
    order[at + 1..]
        .iter()
        .find(|(table, shown)| *shown && !(skip_inside && table.starts_with(path)))
        .map_or(After::End, |(table, _)| After::Header(table.clone()))
}

/// Every table that could print a `[header]`, in the order the file prints
/// them, and whether it does. This is toml_edit's own order: by position, and
/// a table with none (a new one) after the table before it.
fn headers(root: &Table) -> Vec<(Vec<String>, bool)> {
    fn walk(
        table: &Table,
        path: &mut Vec<String>,
        last: &mut isize,
        found: &mut Vec<(isize, Vec<String>, bool)>,
    ) {
        if !table.is_dotted() {
            if let Some(position) = table.position() {
                *last = position;
            }
            let shown = !path.is_empty() && prints_header(table);
            found.push((*last, path.clone(), shown));
        }
        for (name, item) in table.iter() {
            if let Item::Table(child) = item {
                path.push(name.to_string());
                walk(child, path, last, found);
                path.pop();
            }
        }
    }
    let mut found = Vec::new();
    walk(root, &mut Vec::new(), &mut 0, &mut found);
    found.sort_by_key(|(position, path, _)| (!path.is_empty(), *position));
    found
        .into_iter()
        .map(|(_, path, shown)| (path, shown))
        .collect()
}

/// Puts what was said around removed lines in front of what follows them.
fn put(doc: &mut DocumentMut, after: &After, said: &Said) {
    match after {
        After::Key(path) => {
            let (name, tables) = path.split_last().expect("a path names a key");
            if let Some(table) = table_at_mut(doc.as_table_mut(), tables)
                && let Some(mut key) = table.key_mut(name)
            {
                let next = raw(key.leaf_decor().prefix(), "").to_string();
                key.leaf_decor_mut().set_prefix(before(said, &next));
                return;
            }
        }
        After::Header(path) => {
            if let Some(Item::Table(table)) = item_at_mut(doc.as_table_mut(), path) {
                let next = raw(table.decor().prefix(), "\n").to_string();
                table.decor_mut().set_prefix(before(said, &next));
                return;
            }
        }
        After::End => {}
    }
    // A blank line alone is not worth ending the file with.
    if has_comment(&said.lead) || !said.comments.is_empty() {
        let next = doc.trailing().as_str().unwrap_or_default().to_string();
        doc.set_trailing(before(said, &next));
    }
}

/// The text before the line that now follows removed ones: the comments that
/// were written above and beside them, then its own. A blank line that set
/// the removed lines apart is kept, once.
fn before(said: &Said, next: &str) -> String {
    let lines_end = said.lead.rfind('\n').map_or(0, |at| at + 1);
    let (lines, indent) = said.lead.split_at(lines_end);
    if !has_comment(lines) && said.comments.is_empty() {
        return if next.contains('\n') {
            next.to_string()
        } else {
            format!("{lines}{next}")
        };
    }
    let mut text = lines.to_string();
    for comment in &said.comments {
        text.push_str(indent);
        text.push_str(comment);
        text.push('\n');
    }
    text + next
}

fn has_comment(text: &str) -> bool {
    text.lines().any(|line| line.trim_start().starts_with('#'))
}

fn comment_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn raw<'a>(text: Option<&'a RawString>, default: &'a str) -> &'a str {
    text.and_then(RawString::as_str).unwrap_or(default)
}

fn item_at<'a>(root: &'a Table, path: &[String]) -> Option<&'a Item> {
    let (name, tables) = path.split_last()?;
    table_at(root, tables)?.as_table_like()?.get(name)
}

fn item_at_mut<'a>(root: &'a mut Table, path: &[String]) -> Option<&'a mut Item> {
    let (name, tables) = path.split_last()?;
    table_at_mut(root, tables)?.get_mut(name)
}

/// The table at `path` (the top of the file for none), as a table of any
/// shape.
fn table_at<'a>(root: &'a Table, path: &[String]) -> Option<TableRef<'a>> {
    let mut here = TableRef::Table(root);
    for name in path {
        here = match here.as_table_like()?.get(name)? {
            Item::Table(table) => TableRef::Table(table),
            Item::Value(Value::InlineTable(table)) => TableRef::Inline(table),
            _ => return None,
        };
    }
    Some(here)
}

fn table_at_mut<'a>(root: &'a mut Table, path: &[String]) -> Option<&'a mut dyn TableLike> {
    let mut here: &mut dyn TableLike = root;
    for name in path {
        here = here.get_mut(name)?.as_table_like_mut()?;
    }
    Some(here)
}

/// A table of either kind, read-only: `TableLike` alone cannot say where a
/// key's comment is.
#[derive(Clone, Copy)]
enum TableRef<'a> {
    Table(&'a Table),
    Inline(&'a InlineTable),
}

impl<'a> TableRef<'a> {
    fn as_table_like(self) -> Option<&'a dyn TableLike> {
        Some(match self {
            TableRef::Table(table) => table,
            TableRef::Inline(table) => table,
        })
    }

    fn key(self, name: &str) -> Option<&'a toml_edit::Key> {
        match self {
            TableRef::Table(table) => table.key(name),
            TableRef::Inline(table) => table.key(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::exit::Exit;
    use crate::model::HarnessId;

    fn set(path: &str, value: impl Into<Value>) -> Change {
        Change::Set {
            path: KeyPath::of(path),
            value: value.into(),
        }
    }

    fn remove(path: &str) -> Change {
        Change::Remove {
            path: KeyPath::of(path),
        }
    }

    /// `changes` made to a file that says `text`: what it says after.
    fn edited(text: &str, changes: &[Change]) -> String {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        fs::write(&file, text).unwrap();
        apply(&file, changes).unwrap_or_else(|fail| panic!("{}: {text}", fail.message));
        fs::read_to_string(&file).unwrap()
    }

    /// How `World::configure` writes the test suite's configs.
    const DOTTED: &str = "schema = 1\n\
        harness.claude.binary = \"/w/bin/claude\"\n\
        harness.codex.binary = \"/w/bin/codex\"\n\
        limits.int_grace_secs = 1\n\
        limits.term_grace_secs = 1\n";

    #[test]
    fn dotted_keys_stay_dotted_keys() {
        let on = edited(DOTTED, &[set("harness.codex.cap", 60_i64)]);
        assert_eq!(
            on,
            DOTTED.replace("codex\"\n", "codex\"\nharness.codex.cap = 60\n")
        );
        // A table that is new among dotted keys is dotted keys too.
        let capped = edited(
            DOTTED,
            &[set("limits.max_depth", 2_i64), set("review.enabled", true)],
        );
        assert!(
            capped.contains("limits.term_grace_secs = 1\nlimits.max_depth = 2\n"),
            "{capped}"
        );
        assert!(capped.ends_with("\n[review]\nenabled = true\n"), "{capped}");
        // And taking out what was put in gives back the file it was.
        assert_eq!(edited(&on, &[remove("harness.codex.cap")]), DOTTED);
    }

    #[test]
    fn headers_stay_headers_and_a_new_table_gets_one() {
        let text = "schema = 1\n\n[harness.codex]\ncap = 80\n";
        assert_eq!(
            edited(text, &[set("harness.codex.cap", 60_i64)]),
            "schema = 1\n\n[harness.codex]\ncap = 60\n"
        );
        assert_eq!(
            edited(text, &[set("harness.claude.cap", 50_i64)]),
            "schema = 1\n\n[harness.codex]\ncap = 80\n\n[harness.claude]\ncap = 50\n",
            "the new table goes after its sibling, and `harness` stays implicit"
        );
        // Its last key gone, a table with nothing to say goes too.
        assert_eq!(edited(text, &[remove("harness.codex.cap")]), "schema = 1\n");
    }

    #[test]
    fn inline_tables_stay_inline() {
        let text = "schema = 1\nharness.codex = { cap = 80 }\n";
        let more = edited(
            text,
            &[
                set("harness.codex.cap", 60_i64),
                set("harness.codex.max_concurrent", 2_i64),
            ],
        );
        assert_eq!(
            more,
            "schema = 1\nharness.codex = { cap = 60, max_concurrent = 2 }\n"
        );
        assert_eq!(
            edited(&more, &[remove("harness.codex.max_concurrent")]),
            "schema = 1\nharness.codex = { cap = 60 }\n",
            "the space before the brace stays"
        );
        assert_eq!(
            edited(
                &more,
                &[
                    remove("harness.codex.cap"),
                    remove("harness.codex.max_concurrent")
                ]
            ),
            "schema = 1\n"
        );
    }

    const COMMENTED: &str = "# cahoots, by hand\n\
        schema = 1\n\
        \n\
        # Codex costs more: kept lower.\n\
        [harness.codex]\n\
        # busy weeks\n\
        cap = 60  # was 80\n\
        max_concurrent = 2\n\
        \n\
        [limits]\n\
        wait_secs = 120  # two minutes\n";

    #[test]
    fn a_changed_value_keeps_its_comments_and_so_does_the_rest_of_the_file() {
        assert_eq!(
            edited(COMMENTED, &[set("harness.codex.cap", 50_i64)]),
            COMMENTED.replace("cap = 60  # was 80", "cap = 50  # was 80")
        );
    }

    #[test]
    fn a_removed_key_leaves_its_comments_above_what_follows_it() {
        let text = edited(COMMENTED, &[remove("harness.codex.cap")]);
        assert_eq!(
            text,
            COMMENTED.replace(
                "# busy weeks\ncap = 60  # was 80\n",
                "# busy weeks\n# was 80\n"
            )
        );
        // The table is empty now, but it has a comment of its own: it stays,
        // and the comments move on to the next header.
        let text = edited(&text, &[remove("harness.codex.max_concurrent")]);
        assert!(
            text.contains(
                "# Codex costs more: kept lower.\n[harness.codex]\n# busy weeks\n# was 80\n\n[limits]\n"
            ),
            "{text}"
        );
        // Those comments are `[limits]`'s now, as the file reads: it keeps its
        // header when its last line goes, and that line's comment ends the file.
        let text = edited(&text, &[remove("limits.wait_secs")]);
        assert!(
            text.ends_with("# busy weeks\n# was 80\n\n[limits]\n# two minutes\n"),
            "{text}"
        );
        assert!(
            text.starts_with("# cahoots, by hand\nschema = 1\n"),
            "{text}"
        );
    }

    #[test]
    fn a_table_with_nothing_left_and_nothing_said_goes() {
        let text = "schema = 1\n\n[limits]\nwait_secs = 120  # two minutes\n";
        assert_eq!(
            edited(text, &[remove("limits.wait_secs")]),
            "schema = 1\n# two minutes\n"
        );
    }

    #[test]
    fn a_whole_table_can_go_and_its_comments_stay() {
        let text = "schema = 1\n\n\
            # Advice from Claude first.\n\
            [roles.advise]\n\
            candidates = [{ harness = \"claude\", model = \"opus\", effort = \"high\" }]\n\
            calibrate = true  # let it learn\n\
            \n\
            [limits]\n\
            wait_secs = 120\n";
        assert_eq!(
            edited(text, &[remove("roles.advise")]),
            "schema = 1\n\n\
             # Advice from Claude first.\n\
             # let it learn\n\
             \n\
             [limits]\n\
             wait_secs = 120\n"
        );
    }

    #[test]
    fn a_new_file_starts_with_the_schema_and_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config").join("config.toml");
        let config = apply(&file, &[set("harness.codex.cap", 60_i64)]).unwrap();
        assert_eq!(config.harness[&HarnessId::Codex].cap, Some(60));
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "schema = 1\n\n[harness.codex]\ncap = 60\n"
        );
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
        // A file someone left readable by others is private once cahoots
        // writes it.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        apply(&file, &[set("harness.codex.cap", 55_i64)]).unwrap();
        assert_eq!(mode(&file), 0o600);
    }

    #[test]
    fn a_refused_edit_leaves_the_file_as_it_was() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        for (text, change, why) in [
            (
                "schema = 1\n[harness.codex]\ncap = 80\n",
                set("harness.codex.abort_at", 70_i64),
                "must be above the cap",
            ),
            (
                "schema = 1\n[harness.codex]\ncap = 80\n",
                set("harness.codex.cap", 101_i64),
                "must be 1–100",
            ),
            (
                "schema = 1\n[harness.codex\ncap = 80\n",
                set("harness.codex.cap", 60_i64),
                "does not parse at line 2",
            ),
            (
                "schema = 1\nharness = 3\n",
                set("harness.codex.cap", 60_i64),
                "`harness` is not a table",
            ),
        ] {
            fs::write(&file, text).unwrap();
            let fail = apply(&file, &[change]).unwrap_err();
            assert_eq!(fail.exit, Exit::Config);
            assert!(fail.message.contains(why), "{}", fail.message);
            assert_eq!(fs::read_to_string(&file).unwrap(), text);
        }
    }

    #[test]
    fn removing_what_is_not_there_writes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        fs::write(&file, COMMENTED).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        apply(
            &file,
            &[remove("harness.claude.cap"), remove("review.enabled")],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), COMMENTED);
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o644,
            "not even rewritten"
        );
    }

    #[test]
    fn a_link_stays_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dotfiles = tmp.path().join("dotfiles");
        fs::create_dir(&dotfiles).unwrap();
        let real = dotfiles.join("cahoots.toml");
        fs::write(&real, "schema = 1\n").unwrap();
        let config = tmp.path().join("config");
        fs::create_dir(&config).unwrap();
        let link = config.join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        apply(&link, &[set("limits.max_depth", 2_i64)]).unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(&real).unwrap(),
            "schema = 1\n\n[limits]\nmax_depth = 2\n"
        );
        assert_eq!(
            fs::read_dir(&dotfiles).unwrap().count(),
            1,
            "no temp file left"
        );
    }
}
