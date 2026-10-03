//! `refresh`, driven through the binary with stdin and stdout piped — what a
//! session's shell gives it. What `install` wrote first is written by the
//! library (one test installs from a real terminal), in this world's
//! throwaway home.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use cahoots::config::UserConfig;
use cahoots::dirs::Dirs;
use cahoots::install::files::install;
use cahoots::model::HarnessId;
use cahoots::registry::Registry;
use common::{Answer, World, answer};
use serde_json::Value;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const OLD: &str = "0.0.1-old";

fn kind(name: &str) -> String {
    format!(
        "[kinds.{name}]\ndescription = \"Review Rust.\"\nrole = \"review\"\n\
         candidates = [{{ harness = \"claude\", model = \"m\", effort = \"high\" }}, \
         {{ harness = \"codex\", model = \"m\", effort = \"high\" }}]\n"
    )
}

/// A world whose home has both harnesses and the shared skills location,
/// and whose config.toml defines `kinds`.
fn world(kinds: &[&str]) -> World {
    let world = World::new();
    define(&world, kinds);
    for dir in [".claude", ".codex", ".agents/skills"] {
        fs::create_dir_all(world.home.join(dir)).unwrap();
    }
    world
}

fn define(world: &World, kinds: &[&str]) {
    world.configure(&kinds.iter().map(|name| kind(name)).collect::<String>());
}

fn dirs(world: &World) -> Dirs {
    Dirs {
        home: world.home.clone(),
        config: world.config.clone(),
        state: world.state.clone(),
        overridden: true,
    }
}

/// What `install` writes, from this world's config.toml, as a terminal
/// install would: through the library.
fn installed(world: &World, only: Option<HarnessId>) {
    let config = UserConfig::load(&world.config.join("config.toml")).unwrap();
    let kinds = Registry::effective(&config).kinds;
    install(&dirs(world), &kinds, only, false).unwrap();
}

fn refresh(world: &World) -> Answer {
    answer(world.cahoots().arg("refresh"))
}

/// The report for the file at `rel` (under the home), found by its path.
fn report<'a>(answer: &'a Answer, world: &World, rel: &str) -> &'a Value {
    let path = world.home.join(rel);
    answer.data()["files"]
        .as_array()
        .expect("data.files")
        .iter()
        .find(|report| report["path"].as_str() == path.to_str())
        .unwrap_or_else(|| panic!("no report for {rel}: {}", answer.json))
}

fn outcome(answer: &Answer, world: &World, rel: &str) -> String {
    report(answer, world, rel)["outcome"]
        .as_str()
        .unwrap()
        .to_string()
}

fn why(answer: &Answer, world: &World, rel: &str) -> String {
    let report = report(answer, world, rel);
    assert_eq!(report["outcome"], "skipped", "{rel}: {report}");
    report["why"].as_str().unwrap().to_string()
}

fn listed(world: &World) -> Vec<PathBuf> {
    let text = fs::read_to_string(world.state.join("install-manifest.json")).unwrap();
    serde_json::from_str::<Value>(&text).unwrap()["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| PathBuf::from(path.as_str().unwrap()))
        .collect()
}

/// Every entry under `root`, never following a link: a file's bytes and
/// when it was last written, a link's target, a directory's mode.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let seen = if meta.file_type().is_symlink() {
                format!("link → {}", fs::read_link(&path).unwrap().display())
            } else if meta.is_dir() {
                pending.push(path.clone());
                format!("dir {:o}", meta.permissions().mode())
            } else {
                // The time too: a file written again with the same bytes was
                // still written.
                format!(
                    "{:?} {}",
                    meta.modified().unwrap(),
                    String::from_utf8_lossy(&fs::read(&path).unwrap())
                )
            };
            found.insert(path, seen);
        }
    }
    found
}

fn everything(world: &World) -> [BTreeMap<PathBuf, String>; 3] {
    [&world.home, &world.config, &world.state].map(|root| snapshot(root))
}

/// Rewrites the file's stamp line as an older version wrote it.
fn age(path: &Path) {
    let text = fs::read_to_string(path).unwrap();
    let aged: String = text
        .lines()
        .map(|line| {
            if line.starts_with("cahoots_version") || line.starts_with("# cahoots_version") {
                line.replace(&format!("\"{VERSION}\""), &format!("\"{OLD}\""))
            } else {
                line.to_string()
            }
        })
        .map(|line| line + "\n")
        .collect();
    assert_ne!(aged, text, "{} has no stamp to age", path.display());
    fs::write(path, aged).unwrap();
}

/// Deletes the file's stamp line, as a person who made it theirs would.
fn adopt(path: &Path) {
    let text = fs::read_to_string(path).unwrap();
    let adopted: String = text
        .lines()
        .filter(|line| {
            !line.starts_with("cahoots_version") && !line.starts_with("# cahoots_version")
        })
        .map(|line| format!("{line}\n"))
        .collect();
    assert_ne!(adopted, text);
    fs::write(path, adopted).unwrap();
}

/// A stamped file a person edited: the same version, different bytes.
fn edit(path: &Path) {
    let mut text = fs::read_to_string(path).unwrap();
    text.push_str("\nedited by hand\n");
    fs::write(path, text).unwrap();
}

const CLAUDE_DELEGATE: &str = ".claude/agents/cahoots-delegate.md";
const CODEX_DELEGATE: &str = ".codex/agents/cahoots-delegate.toml";
const CLAUDE_SKILL: &str = ".claude/skills/cahoots/SKILL.md";
const CODEX_SKILL: &str = ".codex/skills/cahoots/SKILL.md";

fn claude_kind(name: &str) -> String {
    format!(".claude/agents/cahoots-kind-{name}.md")
}

fn codex_kind(name: &str) -> String {
    format!(".codex/agents/cahoots-kind-{name}.toml")
}

const NOT_INSTALLED_FIXED: &str =
    "`cahoots install` did not write it — run it from a terminal to add it";
const NOT_STAMPED: &str = "exists and is not cahoots' (no cahoots_version stamp) — left alone";
const NOT_REGULAR: &str = "exists and is not a regular file — left alone";
const GONE: &str = "gone — `cahoots install`, from a terminal, writes it again";

#[test]
fn refresh_runs_without_a_terminal_and_rewrites_what_install_wrote() {
    let world = world(&["rust-review"]);
    let at_terminal = world.at_terminal(&["install", "--meter", "none"]).finish();
    assert_eq!(at_terminal.code, 0, "{}", at_terminal.screen);
    age(&world.home.join(CLAUDE_SKILL));
    edit(&world.home.join(CLAUDE_DELEGATE));

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    assert_eq!(refreshed.json["class"], "ok");
    assert_eq!(refreshed.json["retry"], "never");
    assert!(refreshed.json.get("message").is_none_or(Value::is_null));
    let keys: Vec<&String> = refreshed.data().as_object().unwrap().keys().collect();
    assert_eq!(keys, ["files"]);
    let skill = report(&refreshed, &world, CLAUDE_SKILL);
    assert_eq!(skill["outcome"], "updated");
    assert_eq!(skill["from"], OLD);
    assert_eq!(outcome(&refreshed, &world, CLAUDE_DELEGATE), "refreshed");
    for rel in [
        CODEX_SKILL,
        CODEX_DELEGATE,
        ".agents/skills/cahoots/SKILL.md",
        &claude_kind("rust-review"),
        &codex_kind("rust-review"),
    ] {
        assert_eq!(outcome(&refreshed, &world, rel), "up_to_date", "{rel}");
    }
    let skill_text = world.ask(&["skill"]).data()["skill"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        fs::read_to_string(world.home.join(CLAUDE_SKILL)).unwrap(),
        skill_text
    );
    let delegate = fs::read_to_string(world.home.join(CLAUDE_DELEGATE)).unwrap();
    assert!(!delegate.contains("edited by hand"));
}

#[test]
fn a_refresh_with_nothing_to_do_changes_no_file() {
    let world = world(&["rust-review"]);
    installed(&world, None);
    let before = everything(&world);

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    for report in refreshed.data()["files"].as_array().unwrap() {
        assert!(
            ["up_to_date", "skipped"].contains(&report["outcome"].as_str().unwrap()),
            "{report}"
        );
    }
    assert_eq!(everything(&world), before);
}

#[test]
fn a_new_kind_gets_its_subagent_and_a_gone_kind_loses_it() {
    let world = world(&["old-kind"]);
    installed(&world, None);
    define(&world, &["new-kind"]);

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    let listed = listed(&world);
    for rel in [claude_kind("new-kind"), codex_kind("new-kind")] {
        assert_eq!(outcome(&refreshed, &world, &rel), "installed", "{rel}");
        let text = fs::read_to_string(world.home.join(&rel)).unwrap();
        assert!(text.contains("cahoots-kind-new-kind"), "{rel}");
        assert!(
            listed.contains(&world.home.join(&rel)),
            "{rel} is not listed"
        );
    }
    for rel in [claude_kind("old-kind"), codex_kind("old-kind")] {
        assert_eq!(outcome(&refreshed, &world, &rel), "removed", "{rel}");
        assert!(!world.home.join(&rel).exists(), "{rel}");
        assert!(!listed.contains(&world.home.join(&rel)), "{rel} is listed");
    }
}

#[test]
fn a_gone_kinds_subagent_someone_adopted_is_kept() {
    let world = world(&["old-kind"]);
    installed(&world, None);
    let adopted = world.home.join(claude_kind("old-kind"));
    adopt(&adopted);
    let theirs = fs::read(&adopted).unwrap();
    define(&world, &[]);

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    assert_eq!(
        why(&refreshed, &world, &claude_kind("old-kind")),
        "no longer carries the cahoots_version stamp — someone made it theirs; left alone"
    );
    assert_eq!(fs::read(&adopted).unwrap(), theirs);
    assert!(listed(&world).contains(&adopted));
    assert_eq!(
        outcome(&refreshed, &world, &codex_kind("old-kind")),
        "removed"
    );
}

#[test]
fn refresh_never_looks_for_a_meter_or_writes_config() {
    let world = world(&[]);
    world.ccusage(serde_json::json!({}), "", None);
    world.meter(serde_json::json!({}), "");
    world.prefix_path(&world.bin);
    let config_file = world.config.join("config.toml");
    let commented = format!(
        "# my settings, my comments\n{}",
        fs::read_to_string(&config_file).unwrap()
    );
    fs::write(&config_file, &commented).unwrap();
    installed(&world, None);
    let settings = world.home.join(".claude/settings.json");
    fs::write(&settings, "{\"permissions\": {\"allow\": []}}\n").unwrap();
    let rules = world.home.join(".codex/rules/default.rules");
    fs::create_dir_all(rules.parent().unwrap()).unwrap();
    fs::write(&rules, "# mine\n").unwrap();
    let meter_file = world.config.join("meter.json");
    assert!(!meter_file.exists());
    edit(&world.home.join(CLAUDE_DELEGATE));

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    assert_eq!(outcome(&refreshed, &world, CLAUDE_DELEGATE), "refreshed");
    assert_eq!(fs::read_to_string(&config_file).unwrap(), commented);
    assert!(!meter_file.exists(), "refresh wrote meter.json");
    assert_eq!(world.meter_calls(), Vec::<String>::new());
    assert_eq!(world.ccusage_calls(), Vec::<String>::new());
    let data = refreshed.data().as_object().unwrap();
    assert_eq!(data.keys().collect::<Vec<_>>(), ["files"]);
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        "{\"permissions\": {\"allow\": []}}\n"
    );
    assert_eq!(fs::read_to_string(&rules).unwrap(), "# mine\n");
}

#[test]
fn refresh_never_writes_into_a_home_install_has_not_written_into() {
    let home_why =
        "`cahoots install` has not written into ~/.codex — run it there from a terminal first";
    // ~/.codex there all along, and ~/.codex made only after the install.
    for codex_after_install in [false, true] {
        let world = world(&["rust-review"]);
        if codex_after_install {
            fs::remove_dir(world.home.join(".codex")).unwrap();
        }
        installed(&world, Some(HarnessId::Claude));
        if codex_after_install {
            fs::create_dir(world.home.join(".codex")).unwrap();
        }

        let refreshed = refresh(&world);

        assert_eq!(refreshed.code, 0, "{}", refreshed.json);
        assert_eq!(
            snapshot(&world.home.join(".codex")),
            BTreeMap::new(),
            "something was written into ~/.codex"
        );
        assert_eq!(
            why(&refreshed, &world, &codex_kind("rust-review")),
            home_why
        );
        for rel in [CODEX_SKILL, CODEX_DELEGATE] {
            assert_eq!(why(&refreshed, &world, rel), NOT_INSTALLED_FIXED, "{rel}");
        }
        assert_eq!(
            outcome(&refreshed, &world, &claude_kind("rust-review")),
            "up_to_date"
        );
    }
}

#[test]
fn a_file_that_lost_its_stamp_is_left_alone() {
    let world = world(&[]);
    installed(&world, None);
    let delegate = world.home.join(CLAUDE_DELEGATE);
    adopt(&delegate);
    let theirs = fs::read(&delegate).unwrap();

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    assert_eq!(why(&refreshed, &world, CLAUDE_DELEGATE), NOT_STAMPED);
    assert_eq!(fs::read(&delegate).unwrap(), theirs);
}

#[test]
fn a_file_install_did_not_write_is_left_alone() {
    let world = world(&[]);
    installed(&world, Some(HarnessId::Claude));
    // (a) a stamped copy where a fixed file goes, in a home install skipped
    let copy = world.home.join(CODEX_DELEGATE);
    fs::create_dir_all(copy.parent().unwrap()).unwrap();
    let stamped = format!("# cahoots_version = \"{OLD}\"\nname = \"cahoots-delegate\"\n");
    fs::write(&copy, &stamped).unwrap();
    // (b) a file already where a new kind's subagent goes, stamped or not
    define(&world, &["new-kind"]);
    let in_the_way = world.home.join(claude_kind("new-kind"));
    fs::write(
        &in_the_way,
        format!("---\ncahoots_version: \"{OLD}\"\n---\n"),
    )
    .unwrap();
    let unstamped = world.home.join(".claude/agents/cahoots-kind-other-kind.md");
    fs::write(&unstamped, "mine\n").unwrap();
    // (c) a manifest edited to list a stamped file somewhere else in home
    let notes = world.home.join("notes/x.md");
    fs::create_dir_all(notes.parent().unwrap()).unwrap();
    fs::write(&notes, format!("cahoots_version: \"{OLD}\"\nmy notes\n")).unwrap();
    let manifest_file = world.state.join("install-manifest.json");
    let mut manifest: Value =
        serde_json::from_str(&fs::read_to_string(&manifest_file).unwrap()).unwrap();
    manifest["files"]
        .as_array_mut()
        .unwrap()
        .push(notes.to_str().unwrap().into());
    fs::write(&manifest_file, manifest.to_string()).unwrap();

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    assert_eq!(why(&refreshed, &world, CODEX_DELEGATE), NOT_INSTALLED_FIXED);
    assert_eq!(fs::read_to_string(&copy).unwrap(), stamped);
    assert_eq!(
        why(&refreshed, &world, &claude_kind("new-kind")),
        "exists, and `cahoots install` did not write it — left alone"
    );
    assert_eq!(
        fs::read_to_string(&in_the_way).unwrap(),
        format!("---\ncahoots_version: \"{OLD}\"\n---\n")
    );
    assert!(!listed(&world).contains(&in_the_way));
    assert_eq!(
        why(&refreshed, &world, "notes/x.md"),
        "this version does not write it — `cahoots install`, from a terminal, removes it"
    );
    assert_eq!(
        fs::read_to_string(&notes).unwrap(),
        format!("cahoots_version: \"{OLD}\"\nmy notes\n")
    );
    assert!(listed(&world).contains(&notes));
    assert_eq!(fs::read_to_string(&unstamped).unwrap(), "mine\n");

    // The same file in the way, unstamped this time.
    fs::write(&in_the_way, "mine too\n").unwrap();
    let again = refresh(&world);
    assert_eq!(
        why(&again, &world, &claude_kind("new-kind")),
        "exists, and `cahoots install` did not write it — left alone"
    );
    assert_eq!(fs::read_to_string(&in_the_way).unwrap(), "mine too\n");
    assert!(!listed(&world).contains(&in_the_way));
}

#[test]
fn a_listed_file_that_is_gone_is_not_written_back() {
    let world = world(&[]);
    installed(&world, None);
    fs::remove_file(world.home.join(CLAUDE_DELEGATE)).unwrap();
    // And a whole directory gone with what was in it.
    fs::remove_dir_all(world.home.join(".codex/agents")).unwrap();

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    for rel in [CLAUDE_DELEGATE, CODEX_DELEGATE] {
        assert_eq!(why(&refreshed, &world, rel), GONE, "{rel}");
        assert!(!world.home.join(rel).exists(), "{rel} was written back");
        assert!(listed(&world).contains(&world.home.join(rel)), "{rel}");
    }
    assert!(!world.home.join(".codex/agents").exists());
}

#[test]
fn a_link_in_place_of_an_installed_file_is_never_followed() {
    let world = world(&[]);
    installed(&world, None);
    let outside = world.root.join("outside/theirs.md");
    let inside = world.home.join("elsewhere/mine.md");
    for target in [&outside, &inside] {
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, format!("cahoots_version: \"{OLD}\"\n")).unwrap();
    }
    for (rel, target) in [(CLAUDE_DELEGATE, &outside), (CODEX_DELEGATE, &inside)] {
        let path = world.home.join(rel);
        fs::remove_file(&path).unwrap();
        symlink(target, &path).unwrap();
    }

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    for (rel, target) in [(CLAUDE_DELEGATE, &outside), (CODEX_DELEGATE, &inside)] {
        assert_eq!(why(&refreshed, &world, rel), NOT_REGULAR, "{rel}");
        let path = world.home.join(rel);
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            fs::read_to_string(target).unwrap(),
            format!("cahoots_version: \"{OLD}\"\n")
        );
    }
}

#[test]
fn an_agent_home_that_points_out_of_home_is_skipped_and_the_rest_refreshed() {
    let world = world(&["rust-review"]);
    installed(&world, None);
    let agents = world.home.join(".codex/agents");
    let outside = world.root.join("outside-agents");
    fs::rename(&agents, &outside).unwrap();
    symlink(&outside, &agents).unwrap();
    for name in ["cahoots-delegate.toml", "cahoots-kind-rust-review.toml"] {
        age(&outside.join(name));
    }
    let before = snapshot(&outside);
    edit(&world.home.join(CLAUDE_DELEGATE));

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    for rel in [CODEX_DELEGATE, &codex_kind("rust-review")] {
        let why = why(&refreshed, &world, rel);
        assert!(
            why.starts_with(&format!("{} resolves to ", agents.display()))
                && why.ends_with(", outside your home — left alone"),
            "{rel}: {why}"
        );
    }
    assert_eq!(snapshot(&outside), before);
    assert_eq!(outcome(&refreshed, &world, CLAUDE_DELEGATE), "refreshed");
}

#[test]
fn a_kind_name_that_is_a_path_is_refused_before_anything_is_written() {
    let world = world(&[]);
    installed(&world, None);
    world.configure(
        "[kinds.\"../evil\"]\ndescription = \"x\"\nrole = \"review\"\n\
         candidates = [{ harness = \"codex\", model = \"m\", effort = \"high\" }]\n",
    );
    edit(&world.home.join(CLAUDE_DELEGATE));
    let before = everything(&world);

    let refused = refresh(&world);

    assert_eq!(refused.code, 34, "{}", refused.json);
    assert_eq!(everything(&world), before);
}

#[test]
fn refresh_before_any_install_is_refused() {
    let nothing =
        "nothing has been installed here yet: run `cahoots install` from a terminal first";
    let world = world(&[]);
    let manifest_file = world.state.join("install-manifest.json");
    for manifest in [None, Some("{\"v\":1,\"files\":[]}"), Some("not json")] {
        if let Some(text) = manifest {
            fs::write(&manifest_file, text).unwrap();
        }
        let before = everything(&world);

        let refused = refresh(&world);

        assert_eq!(refused.code, 34, "{manifest:?}: {}", refused.json);
        assert_eq!(refused.json["class"], "config_error");
        assert_eq!(refused.json["retry"], "fix_config");
        if manifest == Some("not json") {
            let message = refused.message();
            assert!(
                message.starts_with(&format!("cannot read {}: ", manifest_file.display()))
                    && message
                        .ends_with(" — run `cahoots install` from a terminal to write it again"),
                "{message}"
            );
        } else {
            assert_eq!(refused.message(), nothing, "{manifest:?}");
        }
        assert_eq!(everything(&world), before, "{manifest:?}");
    }
}

#[test]
fn refresh_takes_no_flags_and_ignores_the_callers_environment() {
    let world = world(&[]);
    installed(&world, None);
    edit(&world.home.join(CLAUDE_DELEGATE));
    let before = everything(&world);
    for flag in ["--meter none", "--harness codex", "--dry-run"] {
        let mut argv = vec!["refresh"];
        argv.extend(flag.split(' '));
        let refused = world.ask(&argv);
        assert_eq!(refused.code, 2, "{flag}: {}", refused.json);
        assert_eq!(everything(&world), before, "{flag}");
    }

    let other = world.root.join("other");
    for rel in [CLAUDE_DELEGATE, CODEX_DELEGATE] {
        let copy = other.join(rel);
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::copy(world.home.join(rel), &copy).unwrap();
        age(&copy);
    }
    let theirs = snapshot(&other);
    let refreshed = answer(
        world
            .cahoots()
            .arg("refresh")
            .env("HOME", &other)
            .env("CODEX_HOME", other.join(".codex"))
            .env("CLAUDE_CONFIG_DIR", other.join(".claude")),
    );

    assert_eq!(refreshed.code, 0, "{}", refreshed.json);
    assert_eq!(outcome(&refreshed, &world, CLAUDE_DELEGATE), "refreshed");
    assert_eq!(snapshot(&other), theirs);
}

/// Puts a directory's mode back however the test ends, so the tempdir can go.
struct Mode<'a>(&'a Path, u32);

impl Drop for Mode<'_> {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(self.1));
    }
}

#[test]
fn a_file_that_cannot_be_written_is_reported_and_the_rest_done() {
    if nix::unistd::geteuid().is_root() {
        return; // root writes through any mode
    }
    let world = world(&[]);
    installed(&world, None);
    edit(&world.home.join(CLAUDE_DELEGATE));
    age(&world.home.join(CLAUDE_SKILL));
    // A new kind as well: its codex subagent is added, so the manifest
    // changes, and is saved, even though its claude one cannot be.
    define(&world, &["new-kind"]);
    let agents = world.home.join(".claude/agents");
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o555)).unwrap();
    let _restore = Mode(&agents, 0o755);

    let refreshed = refresh(&world);

    assert_eq!(refreshed.code, 1, "{}", refreshed.json);
    assert_eq!(refreshed.json["class"], "internal_error");
    assert_eq!(
        refreshed.message(),
        "2 of the files could not be changed: see data.files"
    );
    for rel in [CLAUDE_DELEGATE, &claude_kind("new-kind")] {
        let why = why(&refreshed, &world, rel);
        assert!(why.starts_with("cannot write"), "{rel}: {why}");
    }
    assert!(
        fs::read_to_string(world.home.join(CLAUDE_DELEGATE))
            .unwrap()
            .contains("edited by hand")
    );
    assert_eq!(outcome(&refreshed, &world, CLAUDE_SKILL), "updated");
    assert_eq!(
        outcome(&refreshed, &world, &codex_kind("new-kind")),
        "installed"
    );
    let listed = listed(&world);
    assert!(listed.contains(&world.home.join(codex_kind("new-kind"))));
    assert!(!listed.contains(&world.home.join(claude_kind("new-kind"))));
}
