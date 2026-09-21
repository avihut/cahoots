//! The usage meters beyond the tracker's `headroom` contract (tests/gate.rs
//! holds that one): ccusage behind the gate and the watchdog, the meter
//! `install` chose, and how `install` finds one and picks — against fake
//! binaries in throwaway directories, never the real ones.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use cahoots::meter::MeterId;
use cahoots::meter::detect::{self, Decision, Found, Places};
use common::{World, fake_harness};
use serde_json::{Value, json};

const LIMITS: &str = "claude_block_tokens = 300_000_000\ncodex_day_tokens = 50_000_000";

/// A run from Codex to Claude Code: the target ccusage measures by the block.
const TO_CLAUDE: [&str; 4] = ["--caller", "codex", "--to", "claude"];

#[test]
fn ccusage_admits_under_a_declared_limit_and_keeps_its_reading() {
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 100_000_000, "projected": 150_000_000}]}),
        "",
        Some(LIMITS),
    );
    let answer = world.run("hello", &TO_CLAUDE);
    assert_eq!(answer.code, 0, "{}", answer.json);
    let reading = &world.record(&answer.run_id())["admission"]["reading"];
    assert_eq!(reading["meter"], "ccusage");
    assert_eq!(reading["tokens"], 100_000_000);
    assert!(
        (reading["percent"].as_f64().unwrap() - 33.33).abs() < 0.01,
        "{reading}"
    );
    assert_eq!(world.ccusage_calls(), ["blocks --active --json --offline"]);
    // Asked from `/`, not from the repository: ccusage reads a config file
    // from its working directory, and a repository must not tune its gate.
    assert_eq!(
        fs::read_to_string(world.bin.join("ccusage.cwd")).unwrap(),
        "/"
    );
}

#[test]
fn ccusage_refuses_over_the_cap_and_on_course_to_pass_the_limit() {
    // 280M of 300M is 93%, over the cap of 75 less a reader's reserve of 3.
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 280_000_000}]}),
        "",
        Some(LIMITS),
    );
    let over = world.run("hello", &TO_CLAUDE);
    assert_eq!(over.code, 24, "{}", over.json);
    assert_eq!(over.json["retry"], "after_reset");
    assert!(
        over.message().contains("cap of 72%") && over.message().contains("93% of the 300M tokens"),
        "{}",
        over.message()
    );

    // 100M so far, heading for 400M before the block ends.
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 100_000_000, "projected": 400_000_000}]}),
        "",
        Some(LIMITS),
    );
    let forecast = world.run("hello", &TO_CLAUDE);
    assert_eq!(forecast.code, 25, "{}", forecast.json);
    assert!(
        forecast
            .message()
            .contains("33% used so far, 400M projected"),
        "{}",
        forecast.message()
    );
    assert!(
        !world.state.join("runs").exists(),
        "a refused run left a record"
    );
}

#[test]
fn claude_codes_own_limit_notice_refuses_with_no_limit_declared() {
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 5_000_000, "limit_notice": "2099-01-01T00:00:00.000Z"}]}),
        "",
        Some(""),
    );
    let answer = world.run("hello", &TO_CLAUDE);
    assert_eq!(answer.code, 24, "{}", answer.json);
    assert!(
        answer.message().contains("hit its usage limit"),
        "{}",
        answer.message()
    );

    // A notice whose reset has passed holds nothing back.
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 5_000_000, "limit_notice": "2001-01-01T00:00:00.000Z"}]}),
        "",
        Some(""),
    );
    let answer = world.run("hello", &TO_CLAUDE);
    assert_eq!(answer.code, 0, "{}", answer.json);
}

#[test]
fn codex_is_measured_only_against_a_declared_limit() {
    let world = World::new();
    world.ccusage(
        json!({"codex": [{"tokens": 90_000_000}]}),
        "",
        Some("claude_block_tokens = 300_000_000"),
    );
    let answer = world.run("hello", &["--to", "codex"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert!(
        world.ccusage_calls().is_empty(),
        "nothing to measure Codex against, so ccusage is not asked"
    );

    // 90M of a declared 50M.
    world.ccusage(json!({"codex": [{"tokens": 90_000_000}]}), "", Some(LIMITS));
    let over = world.run("hello", &["--to", "codex"]);
    assert_eq!(over.code, 24, "{}", over.json);
    assert_eq!(
        world.ccusage_calls(),
        ["codex daily --last 1 --json --offline"]
    );
}

#[test]
fn a_ccusage_that_does_not_answer_is_a_refusal() {
    for plan in [json!({"exit": 1}), json!({"garbage": true})] {
        let world = World::new();
        world.ccusage(plan.clone(), "", Some(LIMITS));
        let answer = world.run("hello", &TO_CLAUDE);
        assert_eq!(answer.code, 13, "{plan}: {}", answer.json);
    }
    let world = World::new();
    world.configure("[meter.ccusage]\nbinary = \"/nonexistent/ccusage\"");
    assert_eq!(world.run("hello", &TO_CLAUDE).code, 13);
}

#[test]
fn a_meter_runs_only_from_a_pinned_path_and_never_on_the_callers_path() {
    // Turned on without a path, and install never recorded one: no PATH
    // lookup — the caller sets PATH, and could put its own `ccusage` first.
    let world = World::new();
    world.ccusage(json!({"claude": [{"tokens": 1_000}]}), "", None);
    world.configure(&format!("[meter.ccusage]\n{LIMITS}"));
    let path = format!("{}:{}", world.bin.display(), std::env::var("PATH").unwrap());
    let unpinned = common::answer(
        world
            .cahoots()
            .env("PATH", &path)
            .args([
                "run", "--role", "advise", "--caller", "codex", "--to", "claude",
            ])
            .arg("--brief")
            .arg(world.brief("hello")),
    );
    assert_eq!(unpinned.code, 13, "{}", unpinned.json);
    assert!(
        unpinned.message().contains("cahoots install"),
        "{}",
        unpinned.message()
    );
    assert!(world.ccusage_calls().is_empty());

    // Pinned, it runs — on a PATH of its own, not the caller's.
    world.ccusage(json!({"claude": [{"tokens": 1_000}]}), "", Some(LIMITS));
    let admitted = common::answer(
        world
            .cahoots()
            .env("PATH", format!("/caller/chose/this:{path}"))
            .args([
                "run", "--role", "advise", "--caller", "codex", "--to", "claude",
            ])
            .arg("--brief")
            .arg(world.brief("hello")),
    );
    assert_eq!(admitted.code, 0, "{}", admitted.json);
    let seen = fs::read_to_string(world.bin.join("ccusage.path")).unwrap();
    assert!(!seen.contains("/caller/chose/this"), "{seen}");
    assert!(
        seen.starts_with(&world.bin.display().to_string()),
        "its own directory first: {seen}"
    );
}

#[test]
fn the_meter_install_chose_is_on_until_the_config_names_one() {
    let world = World::new();
    let ccusage = world.ccusage(
        json!({"claude": [{"tokens": 5_000_000, "limit_notice": "2099-01-01T00:00:00.000Z"}]}),
        "",
        None,
    );
    fs::write(
        world.config.join("meter.json"),
        json!({"v": 1, "meter": "ccusage", "binary": ccusage}).to_string(),
    )
    .unwrap();
    let refused = world.run("hello", &TO_CLAUDE);
    assert_eq!(
        refused.code, 24,
        "the meter install chose is on: {}",
        refused.json
    );

    // A table in the config outranks what install chose.
    world.meter(json!({"guarded": {"code": 0, "percent": 10}}), "");
    let admitted = world.run("hello", &TO_CLAUDE);
    assert_eq!(admitted.code, 0, "{}", admitted.json);
    assert_eq!(world.meter_calls().len(), 1);

    // "No meter" is a choice too.
    world.configure("");
    fs::write(
        world.config.join("meter.json"),
        json!({"v": 1, "meter": null, "binary": null}).to_string(),
    )
    .unwrap();
    assert_eq!(world.run("hello", &TO_CLAUDE).code, 0);
}

#[test]
fn a_ccusage_metered_run_is_stopped_past_its_abort_threshold() {
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 100_000_000}, {"tokens": 285_000_000}]}),
        "harness.claude.abort_at = 90\nlimits.watchdog_secs = 1",
        Some(LIMITS),
    );
    let answer = world.run(
        "FAKE: sleep=120",
        &["--caller", "codex", "--to", "claude", "--wait", "60"],
    );
    assert_eq!(answer.code, 43, "{}", answer.json);
    assert_eq!(answer.data()["state"], "budget");
    assert!(
        answer.message().contains("95"),
        "the reading that stopped it: {}",
        answer.message()
    );
    assert!(
        world.ccusage_calls().len() >= 3,
        "admission, then two readings over"
    );
}

#[test]
fn with_no_limit_declared_ccusage_does_not_watch() {
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 1_000_000}]}),
        "limits.watchdog_secs = 1",
        Some(""),
    );
    let answer = world.run(
        "FAKE: sleep=3\nFAKE: say=finished",
        &["--caller", "codex", "--to", "claude", "--wait", "60"],
    );
    assert_eq!(answer.code, 0, "{}", answer.json);
    assert_eq!(answer.text(), "finished");
    assert_eq!(world.ccusage_calls().len(), 1, "admission only");
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == name)
        .unwrap_or_else(|| panic!("no {name} check in {report}"))
}

#[test]
fn doctor_says_what_ccusage_measures_and_what_it_cannot() {
    let world = World::new();
    world.ccusage(
        json!({"claude": [{"tokens": 150_000_000}], "codex": [{"tokens": 2_000_000}]}),
        "",
        Some("claude_block_tokens = 300_000_000"),
    );
    let report = world.ask(&["doctor"]).json;
    assert_eq!(check(&report, "meter: ccusage")["status"], "ok", "{report}");
    let detail = |name| check(&report, name)["detail"].as_str().unwrap().to_string();
    assert!(detail("meter: ccusage").contains("20.0.23"));
    assert!(detail("meter: ccusage").contains("config file"));
    assert_eq!(check(&report, "meter: claude")["status"], "ok");
    assert!(
        detail("meter: claude").contains("50% of the 300M tokens"),
        "{}",
        detail("meter: claude")
    );
    assert_eq!(check(&report, "meter: codex")["status"], "warn");
    assert!(detail("meter: codex").contains("codex_day_tokens"));

    // A ccusage too old to read is a failed check.
    fs::write(world.bin.join("ccusage.version"), "ccusage 17.2.1").unwrap();
    let old = world.ask(&["doctor"]);
    assert_eq!(old.code, 34);
    assert_eq!(check(&old.json, "meter: ccusage")["status"], "fail");
    assert!(
        check(&old.json, "meter: ccusage")["detail"]
            .as_str()
            .unwrap()
            .contains("older than 20.0.0")
    );
}

// ── install: finding the meters, and choosing ──

/// A throwaway machine: a home, a PATH of one directory, an Applications
/// folder. Nothing a real machine has is looked at.
struct Machine {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    apps: PathBuf,
}

impl Machine {
    fn new() -> Machine {
        let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let machine = Machine {
            home: root.join("home"),
            bin: root.join("bin"),
            apps: root.join("Applications"),
            root,
            _tmp: tmp,
        };
        for dir in [&machine.home, &machine.bin, &machine.apps] {
            fs::create_dir_all(dir).unwrap();
        }
        machine
    }

    fn places(&self) -> Places<'_> {
        Places {
            home: &self.home,
            path: Some(self.bin.as_os_str()),
            applications: vec![self.apps.clone()],
        }
    }

    fn detect(&self) -> Vec<Found> {
        detect::detect(&self.places(), None)
    }

    /// The fake, named `name`, in `dir`; `plan` is written beside it.
    fn fake(&self, dir: &Path, name: &str, plan: Option<Value>) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(name);
        fs::copy(fake_harness(), &path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        if let Some(plan) = plan {
            fs::write(
                path.with_file_name(format!("{name}.plan")),
                plan.to_string(),
            )
            .unwrap();
        }
        path
    }

    /// Agent Usage's app, as its release puts it, answering `headroom` with `code`.
    fn tracker_app(&self, code: i64) -> PathBuf {
        self.fake(
            &self.apps.join("AgentUsage.app/Contents/MacOS"),
            "usage-cli",
            Some(json!({"unguarded": {"code": code, "percent": 12}})),
        )
    }
}

#[test]
fn install_finds_ccusage_on_path_and_agent_usage_in_its_app() {
    let machine = Machine::new();
    let ccusage = machine.fake(&machine.bin, "ccusage", None);
    let usage_cli = machine.tracker_app(0);
    let found = machine.detect();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(found.iter().all(Found::usable), "{found:?}");
    assert_eq!(
        (found[0].meter, &found[0].binary),
        (MeterId::AgentUsage, &usage_cli)
    );
    assert_eq!(
        (found[1].meter, &found[1].binary),
        (MeterId::Ccusage, &ccusage)
    );
    assert_eq!(found[1].version.as_deref(), Some("20.0.23"));
    // Two that could be used: a person picks.
    match detect::decide(None, None, None, &found).unwrap() {
        Decision::Ask { options } => assert_eq!(options.len(), 2),
        other => panic!("expected a question, got {other:?}"),
    }
}

#[test]
fn a_usage_cli_without_headroom_is_found_but_not_offered() {
    let machine = Machine::new();
    let ccusage = machine.fake(&machine.bin, "ccusage", None);
    machine.tracker_app(19);
    let found = machine.detect();
    let tracker = found
        .iter()
        .find(|found| found.meter == MeterId::AgentUsage)
        .unwrap();
    assert!(
        tracker.unusable.as_deref().unwrap().contains("headroom"),
        "{tracker:?}"
    );
    assert_eq!(
        detect::decide(None, None, None, &found).unwrap(),
        Decision::Use {
            meter: MeterId::Ccusage,
            binary: ccusage,
            because: "the only one found".to_string(),
        }
    );
}

#[test]
fn an_old_ccusage_is_found_but_not_offered() {
    let machine = Machine::new();
    let ccusage = machine.fake(&machine.bin, "ccusage", None);
    fs::write(ccusage.with_file_name("ccusage.version"), "ccusage 17.2.1").unwrap();
    let found = machine.detect();
    assert_eq!(found.len(), 1);
    assert!(
        found[0]
            .unusable
            .as_deref()
            .unwrap()
            .contains("older than 20.0.0"),
        "{found:?}"
    );
    assert_eq!(
        detect::decide(None, None, None, &found).unwrap(),
        Decision::NoneFound
    );
}

#[test]
fn the_trackers_launch_agent_leads_to_its_usage_cli() {
    let machine = Machine::new();
    // Built where its source is, as `mise run app` does: not on PATH, not in
    // an Applications folder — but its daemon's launch agent says where.
    let bundle = machine
        .root
        .join("src/tracker/AgentUsage.app/Contents/MacOS");
    let usage_cli = machine.fake(
        &bundle,
        "usage-cli",
        Some(json!({"unguarded": {"code": 0, "percent": 3}})),
    );
    let agents = machine.home.join("Library/LaunchAgents");
    fs::create_dir_all(&agents).unwrap();
    fs::write(
        agents.join("io.github.avihut.usaged.plist"),
        format!(
            "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict>\n<key>ProgramArguments</key>\n\
             <array>\n<string>{}</string>\n</array>\n</dict></plist>\n",
            bundle.join("usaged").display()
        ),
    )
    .unwrap();
    let found = machine.detect();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        (found[0].meter, &found[0].binary, found[0].usable()),
        (MeterId::AgentUsage, &usage_cli, true)
    );
}

#[test]
fn a_meter_anyone_could_have_written_is_not_one() {
    let machine = Machine::new();
    let ccusage = machine.fake(&machine.bin, "ccusage", None);
    fs::set_permissions(&ccusage, fs::Permissions::from_mode(0o777)).unwrap();
    let found = machine.detect();
    assert!(
        found[0].unusable.as_deref().unwrap().contains("writable"),
        "{found:?}"
    );
}

#[test]
fn the_copy_chosen_last_time_is_looked_at_first() {
    let machine = Machine::new();
    machine.fake(&machine.bin, "ccusage", None);
    let chosen = machine.fake(&machine.root.join("elsewhere"), "ccusage", None);
    let found = detect::detect(&machine.places(), Some((MeterId::Ccusage, &chosen)));
    assert_eq!(found[0].binary, chosen);
}
