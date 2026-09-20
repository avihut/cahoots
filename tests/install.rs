//! `install` / `uninstall`, against a throwaway home. The verbs themselves
//! need a terminal, so these drive the library — the same functions, with a
//! `Dirs` that points at a temp directory instead of the real home.

use std::fs;
use std::path::{Path, PathBuf};

use cahoots::dirs::Dirs;
use cahoots::exit::Exit;
use cahoots::install::files::{Outcome, Report, install, uninstall};
use cahoots::model::HarnessId;

struct Home {
    _tmp: tempfile::TempDir,
    dirs: Dirs,
}

/// A home with both harnesses and the shared skills location set up.
fn home() -> Home {
    let home = bare_home();
    for dir in [".claude", ".codex", ".agents/skills"] {
        fs::create_dir_all(home.dirs.home.join(dir)).unwrap();
    }
    home
}

fn bare_home() -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    Home {
        dirs: Dirs {
            home: root.join("home"),
            config: root.join("config"),
            state: root.join("state"),
            overridden: true,
        },
        _tmp: tmp,
    }
}

fn outcome_of<'a>(reports: &'a [Report], suffix: &str) -> &'a Outcome {
    &reports
        .iter()
        .find(|report| report.path.ends_with(suffix))
        .unwrap_or_else(|| panic!("no report for {suffix}"))
        .outcome
}

fn every_file(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

const ALL: [&str; 8] = [
    ".agents/skills/cahoots/SKILL.md",
    ".claude/skills/cahoots/SKILL.md",
    ".claude/agents/cahoots-delegate.md",
    ".codex/skills/cahoots/SKILL.md",
    ".codex/agents/cahoots-delegate.toml",
    ".agents/skills/cahoots-review/SKILL.md",
    ".claude/skills/cahoots-review/SKILL.md",
    ".codex/skills/cahoots-review/SKILL.md",
];

#[test]
fn install_writes_stamped_files_and_is_idempotent() {
    let home = home();
    let reports = install(&home.dirs, None, false).unwrap();
    assert_eq!(reports.len(), ALL.len());
    for path in ALL {
        assert_eq!(*outcome_of(&reports, path), Outcome::Installed, "{path}");
        let text = fs::read_to_string(home.dirs.home.join(path)).unwrap();
        assert!(text.contains("cahoots_version"), "{path} carries no stamp");
        assert!(!text.contains("{{version}}"), "{path} was not rendered");
    }
    // Install IS update: a second run has nothing to do.
    for report in install(&home.dirs, None, false).unwrap() {
        assert_eq!(
            report.outcome,
            Outcome::UpToDate,
            "{}",
            report.path.display()
        );
    }
}

#[test]
fn a_changed_or_older_copy_is_brought_up_to_date() {
    let home = home();
    install(&home.dirs, None, false).unwrap();
    let skill = home.dirs.home.join(ALL[0]);
    let agent = home.dirs.home.join(ALL[2]);

    let edited = fs::read_to_string(&skill).unwrap() + "\nan edit by hand\n";
    fs::write(&skill, edited).unwrap();
    let older = fs::read_to_string(&agent)
        .unwrap()
        .replace(env!("CARGO_PKG_VERSION"), "0.0.0-older");
    fs::write(&agent, older).unwrap();

    let reports = install(&home.dirs, None, false).unwrap();
    assert_eq!(*outcome_of(&reports, ALL[0]), Outcome::Refreshed);
    assert_eq!(
        *outcome_of(&reports, ALL[2]),
        Outcome::Updated {
            from: "0.0.0-older".to_string()
        }
    );
    assert!(
        !fs::read_to_string(&skill)
            .unwrap()
            .contains("an edit by hand")
    );
}

#[test]
fn a_file_that_is_not_cahoots_is_never_overwritten_or_removed() {
    let home = home();
    let theirs = home.dirs.home.join(ALL[2]);
    fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    fs::write(&theirs, "---\nname: cahoots-delegate\n---\nmy own agent\n").unwrap();

    let reports = install(&home.dirs, None, false).unwrap();
    assert!(matches!(
        outcome_of(&reports, ALL[2]),
        Outcome::Skipped { .. }
    ));
    assert!(
        fs::read_to_string(&theirs)
            .unwrap()
            .contains("my own agent")
    );

    uninstall(&home.dirs, false).unwrap();
    assert!(
        theirs.is_file(),
        "uninstall removed a file install never wrote"
    );
}

#[test]
fn a_harness_that_is_not_set_up_is_skipped_not_created() {
    let home = bare_home();
    fs::create_dir_all(home.dirs.home.join(".claude")).unwrap();
    let reports = install(&home.dirs, None, false).unwrap();
    assert_eq!(*outcome_of(&reports, ALL[1]), Outcome::Installed);
    assert!(matches!(
        outcome_of(&reports, ALL[3]),
        Outcome::Skipped { .. }
    ));
    assert!(matches!(
        outcome_of(&reports, ALL[0]),
        Outcome::Skipped { .. }
    ));
    assert!(
        !home.dirs.home.join(".codex").exists(),
        "install invented an agent home"
    );
    assert!(!home.dirs.home.join(".agents").exists());
}

#[test]
fn one_harness_can_be_installed_alone() {
    let home = home();
    let reports = install(&home.dirs, Some(HarnessId::Codex), false).unwrap();
    let paths: Vec<&Path> = reports.iter().map(|r| r.path.as_path()).collect();
    assert_eq!(
        paths.len(),
        5,
        "the two shared skills plus Codex's three files: {paths:?}"
    );
    assert!(!home.dirs.home.join(".claude/agents").exists());
}

#[test]
fn a_dry_run_writes_nothing_at_all() {
    let home = home();
    let before = every_file(&home.dirs.home);
    let reports = install(&home.dirs, None, true).unwrap();
    assert_eq!(
        *outcome_of(&reports, ALL[0]),
        Outcome::Installed,
        "it still says what it would do"
    );
    assert_eq!(every_file(&home.dirs.home), before);
    assert!(!home.dirs.state.exists(), "a dry run wrote a manifest");

    install(&home.dirs, None, false).unwrap();
    let installed = every_file(&home.dirs.home);
    for report in uninstall(&home.dirs, true).unwrap() {
        assert_eq!(report.outcome, Outcome::Removed);
    }
    assert_eq!(every_file(&home.dirs.home), installed);
}

#[test]
fn uninstall_removes_exactly_what_install_wrote() {
    let home = home();
    // Things that were there before, and must be there after.
    let settings = home.dirs.home.join(".claude/settings.json");
    fs::write(&settings, r#"{"permissions":{"defaultMode":"auto"}}"#).unwrap();
    let rules = home.dirs.home.join(".codex/rules/default.rules");
    fs::create_dir_all(rules.parent().unwrap()).unwrap();
    fs::write(
        &rules,
        "prefix_rule(pattern=[\"swift\", \"test\"], decision=\"allow\")\n",
    )
    .unwrap();
    let neighbour = home.dirs.home.join(".claude/skills/other/SKILL.md");
    fs::create_dir_all(neighbour.parent().unwrap()).unwrap();
    fs::write(&neighbour, "someone else's skill").unwrap();
    let before = every_file(&home.dirs.home);

    install(&home.dirs, None, false).unwrap();
    // cahoots prints permission rules; it never writes them.
    assert_eq!(
        fs::read_to_string(&settings).unwrap(),
        r#"{"permissions":{"defaultMode":"auto"}}"#
    );
    assert!(!fs::read_to_string(&rules).unwrap().contains("cahoots"));

    // A person adopts one file: the stamp goes, and with it cahoots' claim.
    let adopted = home.dirs.home.join(ALL[4]);
    fs::write(&adopted, "name = \"cahoots-delegate\"\n# mine now\n").unwrap();

    let reports = uninstall(&home.dirs, false).unwrap();
    assert!(matches!(
        outcome_of(&reports, ALL[4]),
        Outcome::Skipped { .. }
    ));
    assert_eq!(*outcome_of(&reports, ALL[0]), Outcome::Removed);

    let mut expected = before;
    expected.push(adopted);
    expected.sort();
    assert_eq!(every_file(&home.dirs.home), expected);
    assert!(
        !home.dirs.home.join(".claude/skills/cahoots").exists(),
        "an empty cahoots dir was left"
    );
    assert!(home.dirs.home.join(".claude/skills/other").is_dir());
}

#[test]
fn an_agent_home_that_points_out_of_home_is_not_followed() {
    let home = bare_home();
    let outside = home.dirs.home.parent().unwrap().join("elsewhere");
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, home.dirs.home.join(".codex")).unwrap();

    let fail = install(&home.dirs, Some(HarnessId::Codex), false).unwrap_err();
    assert_eq!(fail.exit, Exit::Policy);
    assert!(
        every_file(&outside).is_empty(),
        "something was written through the symlink"
    );
    let left: Vec<_> = fs::read_dir(&outside)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert!(left.is_empty(), "created through the symlink: {left:?}");
}
