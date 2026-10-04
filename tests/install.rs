//! `install` / `uninstall`, against a throwaway home. The verbs themselves
//! need a terminal, so these drive the library — the same functions, with a
//! `Dirs` that points at a temp directory instead of the real home.

use std::fs;
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use cahoots::config::UserConfig;
use cahoots::dirs::Dirs;
use cahoots::exit::Exit;
use cahoots::install::files::{Outcome, Report, Stale, StaleWhy, install, stale, uninstall};
use cahoots::model::{HarnessId, TaskKindName};
use cahoots::registry::{KindEntry, Registry};

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
            data: root.join("data"),
            overridden: true,
        },
        _tmp: tmp,
    }
}

type Kinds = BTreeMap<TaskKindName, KindEntry>;

fn no_kinds() -> Kinds {
    Kinds::new()
}

/// The kinds a config.toml of these `[kinds.*]` tables defines.
fn kinds(tables: &[String]) -> Kinds {
    let text = format!("schema = 1\n{}", tables.concat());
    Registry::effective(&UserConfig::parse(&text).unwrap()).kinds
}

fn kind(name: &str, description: &str, role: &str, on: &[&str]) -> String {
    let candidates: Vec<String> = on
        .iter()
        .map(|harness| format!("{{ harness = \"{harness}\", model = \"m\", effort = \"high\" }}"))
        .collect();
    format!(
        "[kinds.{name}]\ndescription = \"{description}\"\nrole = \"{role}\"\ncandidates = [{}]\n",
        candidates.join(", ")
    )
}

const BOTH: &[&str] = &["claude", "codex"];

fn claude_agent(name: &str) -> String {
    format!(".claude/agents/cahoots-kind-{name}.md")
}

fn codex_agent(name: &str) -> String {
    format!(".codex/agents/cahoots-kind-{name}.toml")
}

fn stale_in(home: &Home, kinds: &Kinds) -> Vec<(String, StaleWhy)> {
    stale(&home.dirs, kinds)
        .into_iter()
        .map(|Stale { path, why }| {
            let path = path.strip_prefix(&home.dirs.home).unwrap();
            (path.display().to_string(), why)
        })
        .collect()
}

fn manifest(home: &Home) -> String {
    fs::read_to_string(home.dirs.state.join("install-manifest.json")).unwrap()
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
    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
    assert_eq!(reports.len(), ALL.len());
    for path in ALL {
        assert_eq!(*outcome_of(&reports, path), Outcome::Installed, "{path}");
        let text = fs::read_to_string(home.dirs.home.join(path)).unwrap();
        assert!(text.contains("cahoots_version"), "{path} carries no stamp");
        assert!(!text.contains("{{version}}"), "{path} was not rendered");
        if path.ends_with("/cahoots/SKILL.md") {
            for words in [
                "data.blind",
                "honest outcome",
                "status <run>",
                "harness stays",
                "aggregate reports",
                "diagnostic text",
                "off by default",
                "as a background command",
                "wait <run> --timeout 1800",
                "keep the loop above",
            ] {
                assert!(text.contains(words), "{path} lacks {words:?}");
            }
        }
        if path.ends_with("/cahoots-review/SKILL.md") {
            for words in [
                "data.next.blind",
                "honest",
                "does not reveal",
                "aggregate reports",
                "harness remains",
            ] {
                assert!(text.contains(words), "{path} lacks {words:?}");
            }
        }
    }
    // Install IS update: a second run has nothing to do.
    for report in install(&home.dirs, &no_kinds(), None, false).unwrap() {
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
    install(&home.dirs, &no_kinds(), None, false).unwrap();
    let skill = home.dirs.home.join(ALL[0]);
    let agent = home.dirs.home.join(ALL[2]);

    let edited = fs::read_to_string(&skill).unwrap() + "\nan edit by hand\n";
    fs::write(&skill, edited).unwrap();
    let older = fs::read_to_string(&agent)
        .unwrap()
        .replace(env!("CARGO_PKG_VERSION"), "0.0.0-older");
    fs::write(&agent, older).unwrap();

    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
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

    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
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
    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
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
    let reports = install(&home.dirs, &no_kinds(), Some(HarnessId::Codex), false).unwrap();
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
    let reports = install(&home.dirs, &no_kinds(), None, true).unwrap();
    assert_eq!(
        *outcome_of(&reports, ALL[0]),
        Outcome::Installed,
        "it still says what it would do"
    );
    assert_eq!(every_file(&home.dirs.home), before);
    assert!(!home.dirs.state.exists(), "a dry run wrote a manifest");

    install(&home.dirs, &no_kinds(), None, false).unwrap();
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

    install(&home.dirs, &no_kinds(), None, false).unwrap();
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

    let fail = install(&home.dirs, &no_kinds(), Some(HarnessId::Codex), false).unwrap_err();
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

#[test]
fn each_kind_gets_a_subagent_in_each_home_and_install_keeps_them_up_to_date() {
    let home = home();
    let kinds = kinds(&[
        kind("rust-review", "Review Rust.", "review", BOTH),
        kind("docs", "Write docs.", "implement", BOTH),
    ]);
    let reports = install(&home.dirs, &kinds, None, false).unwrap();
    assert_eq!(reports.len(), ALL.len() + 4);
    for name in ["rust-review", "docs"] {
        for path in [claude_agent(name), codex_agent(name)] {
            assert_eq!(*outcome_of(&reports, &path), Outcome::Installed, "{path}");
            let text = fs::read_to_string(home.dirs.home.join(&path)).unwrap();
            assert!(text.contains(&format!("cahoots run --kind {name} --caller")));
            assert!(
                manifest(&home).contains(&path),
                "{path} is not in the manifest"
            );
        }
    }
    for report in install(&home.dirs, &kinds, None, false).unwrap() {
        assert_eq!(
            report.outcome,
            Outcome::UpToDate,
            "{}",
            report.path.display()
        );
    }
    assert_eq!(stale_in(&home, &kinds), []);
}

#[test]
fn a_harness_gets_no_subagent_for_a_kind_it_could_not_delegate() {
    let home = home();
    let only_codex = kinds(&[kind("rust-review", "Review Rust.", "review", &["codex"])]);
    let reports = install(&home.dirs, &only_codex, None, false).unwrap();
    assert_eq!(
        *outcome_of(&reports, &claude_agent("rust-review")),
        Outcome::Installed
    );
    let Outcome::Skipped { why } = outcome_of(&reports, &codex_agent("rust-review")) else {
        panic!("Codex got a subagent that could only ever delegate to itself");
    };
    assert!(why.contains("does not delegate to itself"), "{why}");
    assert!(!home.dirs.home.join(codex_agent("rust-review")).exists());
    assert_eq!(stale_in(&home, &only_codex), []);

    // A kind that loses its candidates on the other side loses that subagent.
    let only_claude = kinds(&[kind("rust-review", "Review Rust.", "review", &["claude"])]);
    let reports = install(&home.dirs, &only_claude, None, false).unwrap();
    let about_claude: Vec<&Outcome> = reports
        .iter()
        .filter(|report| report.path.ends_with(claude_agent("rust-review")))
        .map(|report| &report.outcome)
        .collect();
    assert_eq!(
        about_claude,
        [&Outcome::Removed],
        "one report, saying it went"
    );
    assert!(!home.dirs.home.join(claude_agent("rust-review")).exists());
}

#[test]
fn a_kind_named_delegate_leaves_the_delegate_alone() {
    let home = home();
    install(&home.dirs, &no_kinds(), None, false).unwrap();
    let delegate = home.dirs.home.join(".claude/agents/cahoots-delegate.md");
    let before = fs::read_to_string(&delegate).unwrap();
    let kinds = kinds(&[kind("delegate", "Delegate.", "advise", BOTH)]);
    let reports = install(&home.dirs, &kinds, None, false).unwrap();
    assert_eq!(
        *outcome_of(&reports, &claude_agent("delegate")),
        Outcome::Installed
    );
    assert_eq!(
        *outcome_of(&reports, ".claude/agents/cahoots-delegate.md"),
        Outcome::UpToDate
    );
    assert_eq!(fs::read_to_string(&delegate).unwrap(), before);
}

#[test]
fn an_edited_kind_is_stale_until_install_refreshes_it() {
    let home = home();
    let before = kinds(&[kind("rust-review", "Review Rust.", "review", BOTH)]);
    install(&home.dirs, &before, None, false).unwrap();
    let after = kinds(&[kind(
        "rust-review",
        "Review Rust, strictly.",
        "review",
        BOTH,
    )]);
    assert_eq!(
        stale_in(&home, &after),
        [
            (claude_agent("rust-review"), StaleWhy::Changed),
            (codex_agent("rust-review"), StaleWhy::Changed),
        ]
    );
    let reports = install(&home.dirs, &after, None, false).unwrap();
    assert_eq!(
        *outcome_of(&reports, &claude_agent("rust-review")),
        Outcome::Refreshed
    );
    assert_eq!(
        *outcome_of(&reports, &codex_agent("rust-review")),
        Outcome::Refreshed
    );
    assert!(
        fs::read_to_string(home.dirs.home.join(claude_agent("rust-review")))
            .unwrap()
            .contains("strictly")
    );
    assert_eq!(stale_in(&home, &after), []);
}

#[test]
fn a_new_kind_is_not_installed_yet_only_where_install_has_run() {
    let home = home();
    let kinds = kinds(&[kind("rust-review", "Review Rust.", "review", BOTH)]);
    assert_eq!(
        stale_in(&home, &kinds),
        [],
        "someone who never installed is not nagged"
    );

    install(&home.dirs, &no_kinds(), Some(HarnessId::Claude), false).unwrap();
    assert_eq!(
        stale_in(&home, &kinds),
        [(claude_agent("rust-review"), StaleWhy::NotInstalled)]
    );
    install(&home.dirs, &no_kinds(), None, false).unwrap();
    assert_eq!(
        stale_in(&home, &kinds),
        [
            (claude_agent("rust-review"), StaleWhy::NotInstalled),
            (codex_agent("rust-review"), StaleWhy::NotInstalled),
        ]
    );
}

#[test]
fn install_removes_the_subagents_of_a_kind_that_is_gone() {
    let home = home();
    let kinds = kinds(&[
        kind("rust-review", "Review Rust.", "review", BOTH),
        kind("kept", "Keep it.", "advise", BOTH),
    ]);
    install(&home.dirs, &kinds, None, false).unwrap();
    // A person adopts one of them: the stamp goes, and with it cahoots' claim.
    let adopted = home.dirs.home.join(claude_agent("kept"));
    let theirs: String = fs::read_to_string(&adopted)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("cahoots_version"))
        .map(|line| format!("{line}\n"))
        .collect();
    fs::write(&adopted, &theirs).unwrap();

    let gone = [
        claude_agent("rust-review"),
        codex_agent("rust-review"),
        codex_agent("kept"),
    ];
    let mut expected: Vec<(String, StaleWhy)> = gone
        .iter()
        .map(|path| (path.clone(), StaleWhy::NoLongerWanted))
        .collect();
    expected.sort();
    let mut found = stale_in(&home, &no_kinds());
    found.sort();
    assert_eq!(found, expected);

    let reports = install(&home.dirs, &no_kinds(), None, true).unwrap();
    for path in &gone {
        assert_eq!(*outcome_of(&reports, path), Outcome::Removed, "{path}");
        assert!(
            home.dirs.home.join(path).is_file(),
            "a dry run removed {path}"
        );
    }

    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
    for path in &gone {
        assert_eq!(*outcome_of(&reports, path), Outcome::Removed, "{path}");
        assert!(!home.dirs.home.join(path).exists(), "{path} is still there");
        assert!(!manifest(&home).contains(path.as_str()));
    }
    assert!(matches!(
        outcome_of(&reports, &claude_agent("kept")),
        Outcome::Skipped { .. }
    ));
    assert_eq!(fs::read_to_string(&adopted).unwrap(), theirs);
    assert_eq!(stale_in(&home, &no_kinds()), []);
}

#[test]
fn a_one_harness_install_removes_only_that_harness_s_subagents() {
    let home = home();
    let kinds = kinds(&[kind("rust-review", "Review Rust.", "review", BOTH)]);
    install(&home.dirs, &kinds, None, false).unwrap();
    install(&home.dirs, &no_kinds(), Some(HarnessId::Codex), false).unwrap();
    assert!(!home.dirs.home.join(codex_agent("rust-review")).exists());
    assert!(home.dirs.home.join(claude_agent("rust-review")).is_file());
    assert_eq!(
        stale_in(&home, &no_kinds()),
        [(claude_agent("rust-review"), StaleWhy::NoLongerWanted)]
    );
}

#[test]
fn uninstall_removes_the_subagents_and_a_person_s_own_agents_stay() {
    let home = home();
    let agents = home.dirs.home.join(".claude/agents");
    fs::create_dir_all(&agents).unwrap();
    let mine = agents.join("mine.md");
    fs::write(&mine, "---\nname: mine\n---\nmy own agent\n").unwrap();
    // A file of a kind's name that a person wrote is never cahoots' to touch.
    let theirs = home.dirs.home.join(claude_agent("theirs"));
    fs::write(&theirs, "---\nname: cahoots-kind-theirs\n---\nmine\n").unwrap();

    let kinds = kinds(&[
        kind("rust-review", "Review Rust.", "review", BOTH),
        kind("theirs", "Theirs.", "advise", BOTH),
    ]);
    let reports = install(&home.dirs, &kinds, None, false).unwrap();
    assert!(matches!(
        outcome_of(&reports, &claude_agent("theirs")),
        Outcome::Skipped { .. }
    ));
    assert_eq!(stale_in(&home, &kinds), []);
    assert_eq!(
        stale_in(&home, &no_kinds()).len(),
        3,
        "theirs is not among them"
    );

    let reports = uninstall(&home.dirs, false).unwrap();
    for path in [
        claude_agent("rust-review"),
        codex_agent("rust-review"),
        codex_agent("theirs"),
    ] {
        assert_eq!(*outcome_of(&reports, &path), Outcome::Removed, "{path}");
        assert!(!home.dirs.home.join(&path).exists());
    }
    assert!(mine.is_file() && theirs.is_file());
    assert!(agents.is_dir());
}

#[test]
fn a_role_edit_is_stale_until_install_refreshes_it() {
    let home = home();
    install(
        &home.dirs,
        &kinds(&[kind("rust-review", "Review Rust.", "review", BOTH)]),
        None,
        false,
    )
    .unwrap();
    for role in ["advise", "implement"] {
        let edited = kinds(&[kind("rust-review", "Review Rust.", role, BOTH)]);
        assert_eq!(
            stale_in(&home, &edited),
            [
                (claude_agent("rust-review"), StaleWhy::Changed),
                (codex_agent("rust-review"), StaleWhy::Changed),
            ],
            "{role}"
        );
        let reports = install(&home.dirs, &edited, None, false).unwrap();
        assert_eq!(
            *outcome_of(&reports, &claude_agent("rust-review")),
            Outcome::Refreshed,
            "{role}"
        );
        assert_eq!(stale_in(&home, &edited), [], "{role}");
    }
    let claude = fs::read_to_string(home.dirs.home.join(claude_agent("rust-review"))).unwrap();
    assert!(claude.contains("--fork"));
}

/// Removal is confined like a write: an agent home that resolves outside
/// home is never deleted through, by install or by uninstall.
#[test]
fn nothing_is_removed_through_an_agent_home_that_points_out_of_home() {
    let home = home();
    let kinds = kinds(&[kind("rust-review", "Review Rust.", "review", BOTH)]);
    install(&home.dirs, &kinds, None, false).unwrap();
    let agents = home.dirs.home.join(".claude/agents");
    let outside = home.dirs.home.parent().unwrap().join("elsewhere");
    fs::rename(&agents, &outside).unwrap();
    std::os::unix::fs::symlink(&outside, &agents).unwrap();
    let survivor = outside.join("cahoots-kind-rust-review.md");
    let delegate = outside.join("cahoots-delegate.md");

    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
    let Outcome::Skipped { why } = outcome_of(&reports, &claude_agent("rust-review")) else {
        panic!("removed through a symlink out of home");
    };
    assert!(why.contains("outside your home"), "{why}");
    assert!(survivor.is_file());
    assert!(
        manifest(&home).contains(&claude_agent("rust-review")),
        "a file left alone stays listed"
    );
    assert!(!home.dirs.home.join(codex_agent("rust-review")).exists());

    uninstall(&home.dirs, false).unwrap();
    assert!(survivor.is_file() && delegate.is_file());
}

/// Only a file that is really gone leaves the manifest. One that cannot be
/// read stays listed, so a later install or uninstall still finds it.
#[test]
fn an_unreadable_file_is_left_and_stays_listed() {
    use std::os::unix::fs::PermissionsExt;

    let home = home();
    let kinds = kinds(&[kind("rust-review", "Review Rust.", "review", BOTH)]);
    install(&home.dirs, &kinds, None, false).unwrap();
    let unreadable = home.dirs.home.join(codex_agent("rust-review"));
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
    let Outcome::Skipped { why } = outcome_of(&reports, &codex_agent("rust-review")) else {
        panic!("an unreadable file was not left alone");
    };
    assert!(why.contains("cannot be read"), "{why}");
    assert!(unreadable.exists());
    assert!(manifest(&home).contains(&codex_agent("rust-review")));

    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o644)).unwrap();
    let reports = install(&home.dirs, &no_kinds(), None, false).unwrap();
    assert_eq!(
        *outcome_of(&reports, &codex_agent("rust-review")),
        Outcome::Removed
    );
    assert!(!unreadable.exists());
}
