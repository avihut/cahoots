//! `doctor`'s warnings for candidates nothing is known about: current kind
//! lists and the role lists a person wrote, matched against the whole history.
//! Every world is a throwaway one; no harness is real.
mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use common::{Answer, World};
use serde_json::Value;

const DAY: u64 = 24 * 3600;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

const CONFIG: &str = r#"
[roles.review]
candidates = [
  { harness = "codex", model = "a", effort = "high" },
  { harness = "claude", model = "b", effort = "high" },
]
[kinds.rust-review]
description = "Review Rust."
role = "review"
candidates = [
  { harness = "codex", model = "a", effort = "medium" },
  { harness = "codex", model = "a", effort = "high" },
]
"#;

/// A finished run, and its outcome if any, as history lines.
fn lines(
    n: usize,
    t: u64,
    kind: Option<&str>,
    role: &str,
    target: (&str, &str, &str),
    state: &str,
    outcome: &str,
) -> String {
    let run = format!("0198c0de-0000-7000-8000-{n:012}");
    let task_kind = kind
        .map(|name| format!("\"task_kind\":\"{name}\","))
        .unwrap_or_default();
    let (harness, model, effort) = target;
    let mut text = format!(
        "{{\"kind\":\"finished\",\"t\":{t},\"run\":\"{run}\",\"role\":\"{role}\",{task_kind}\
         \"caller\":null,\"target\":{{\"harness\":\"{harness}\",\"model\":\"{model}\",\
         \"effort\":\"{effort}\"}},\"dir\":\"/w\",\"state\":\"{state}\",\"exit\":0,\
         \"tokens_in\":1,\"tokens_out\":1,\"secs\":1,\"sampled\":false}}\n"
    );
    if !outcome.is_empty() {
        text.push_str(&format!(
            "{{\"kind\":\"outcome\",\"t\":{t},\"run\":\"{run}\",\"outcome\":\"{outcome}\"}}\n"
        ));
    }
    text
}

fn history(world: &World, text: &str) {
    fs::create_dir_all(&world.state).unwrap();
    fs::write(world.state.join("history.jsonl"), text).unwrap();
}

/// The evidence warnings among doctor's checks, as `label` strings.
fn warned(answer: &Answer) -> Vec<String> {
    answer.data()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|check| check["check"].as_str().unwrap().ends_with(" evidence"))
        .map(|check| {
            assert_eq!(check["status"], "warn");
            assert_eq!(
                check["detail"],
                "no rated or failed runs on record for this candidate in this list — \
                 not enough evidence"
            );
            check["check"].as_str().unwrap().to_string()
        })
        .collect()
}

fn doctor(world: &World) -> Answer {
    let answer = world.ask(&["doctor"]);
    assert_eq!(answer.code, 0, "{}", answer.json);
    answer
}

const ROLE_A: &str = "role review: codex a high evidence";
const ROLE_B: &str = "role review: claude b high evidence";
const KIND_MEDIUM: &str = "kind rust-review: codex a medium evidence";
const KIND_HIGH: &str = "kind rust-review: codex a high evidence";

#[test]
fn doctor_warns_only_for_missing_list_evidence() {
    let world = World::new();
    world.configure(CONFIG);
    // Nothing on record: the person's role list, then the kinds, each in the
    // order written. Shipped role lists are not mentioned, and claude is
    // enabled here, so say the same of a disabled one below.
    assert_eq!(
        warned(&doctor(&world)),
        [ROLE_A, ROLE_B, KIND_MEDIUM, KIND_HIGH]
    );

    let t = now();
    let a_high = ("codex", "a", "high");
    let a_medium = ("codex", "a", "medium");
    let mut text = String::new();
    // Evidence in other lists or other efforts clears nothing it is not for:
    // a kind run for the role list, a role run for the kind list, another
    // role, another effort.
    text.push_str(&lines(
        0,
        t,
        Some("rust-review"),
        "review",
        a_high,
        "done",
        "accepted",
    ));
    text.push_str(&lines(1, t, None, "review", a_medium, "done", "accepted"));
    text.push_str(&lines(
        2,
        t,
        Some("rust-review"),
        "advise",
        a_medium,
        "done",
        "accepted",
    ));
    // Unknown, cancelled and budget-stopped are not evidence either.
    text.push_str(&lines(3, t, None, "review", a_high, "done", ""));
    text.push_str(&lines(
        4,
        t,
        None,
        "review",
        ("claude", "b", "high"),
        "cancelled",
        "",
    ));
    text.push_str(&lines(
        5,
        t,
        Some("rust-review"),
        "review",
        a_medium,
        "budget",
        "",
    ));
    history(&world, &text);
    // The rated kind run clears KIND_HIGH, and nothing else.
    assert_eq!(warned(&doctor(&world)), [ROLE_A, ROLE_B, KIND_MEDIUM]);

    // One matching observation clears a warning, below any floor — and a
    // run that failed by itself is one; so is an old one, and `learn reset`
    // forgets none of it.
    text.push_str(&lines(
        6,
        t - 400 * DAY,
        None,
        "review",
        a_high,
        "failed",
        "",
    ));
    text.push_str(&lines(
        7,
        t,
        Some("rust-review"),
        "review",
        a_medium,
        "timed_out",
        "",
    ));
    text.push_str(&format!("{{\"kind\":\"forget\",\"t\":{t}}}\n"));
    history(&world, &text);
    assert_eq!(warned(&doctor(&world)), [ROLE_B]);
}

#[test]
fn doctor_leaves_shipped_lists_removed_candidates_and_disabled_ones_straight() {
    // The shipped lists, untouched: nothing to say.
    let world = World::new();
    assert!(warned(&doctor(&world)).is_empty());

    // A disabled harness's candidate in a current kind list is still a gap.
    let world = World::new();
    world.enable(&["codex"]);
    world.configure(CONFIG);
    assert_eq!(warned(&doctor(&world)).len(), 4);
    // A candidate removed from the list is not asked about, whatever history says.
    world.configure(
        "[kinds.rust-review]\ndescription = \"d\"\nrole = \"review\"\n\
         candidates = [{ harness = \"codex\", model = \"a\", effort = \"medium\" }]\n",
    );
    history(
        &world,
        &lines(
            0,
            now(),
            Some("rust-review"),
            "review",
            ("codex", "gone", "low"),
            "failed",
            "",
        ),
    );
    assert_eq!(warned(&doctor(&world)), [KIND_MEDIUM]);
}

/// Every file under `dir` with its bytes: a world's whole state at one moment.
fn fingerprint(dir: &Path, into: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            fingerprint(&path, into);
        } else {
            into.insert(
                path.display().to_string(),
                fs::read(&path).unwrap_or_default(),
            );
        }
    }
}

#[test]
fn doctor_warnings_do_not_fail_or_launch_runs() {
    let world = World::new();
    world.configure(CONFIG);
    let before = |world: &World| {
        let mut files = BTreeMap::new();
        fingerprint(&world.config, &mut files);
        fingerprint(&world.state, &mut files);
        files
    };
    let was = before(&world);
    let answer = doctor(&world);
    assert_eq!(answer.json["class"], "ok");
    assert_eq!(answer.json["retry"], "never");
    assert!(answer.json.get("message").is_none(), "{}", answer.json);
    assert_eq!(warned(&answer).len(), 4);
    assert_eq!(
        before(&world),
        was,
        "doctor changed the config or the state"
    );
    assert!(!world.state.join("runs").exists());
    assert!(!world.state.join("history.jsonl").exists());
    // No runner is advertised: nothing a person could copy to fill the gap.
    for check in answer.data()["checks"].as_array().unwrap() {
        if check["check"].as_str().unwrap().ends_with(" evidence") {
            let detail = check["detail"].as_str().unwrap();
            assert!(
                !detail.contains("cahoots") && !detail.contains("eval"),
                "{detail}"
            );
        }
    }

    // A malformed config is the existing failed check, and no gap checks.
    fs::write(world.config.join("config.toml"), "schema = 1\n[kinds.x\n").unwrap();
    let broken = world.ask(&["doctor"]);
    assert_eq!(broken.code, 34, "{}", broken.json);
    assert_eq!(broken.json["class"], "config_error");
    assert_eq!(broken.json["retry"], "fix_config");
    assert_eq!(broken.json["message"], "1 check(s) failed");
    assert!(warned(&broken).is_empty());
    let config = broken.data()["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "config")
        .unwrap();
    assert_eq!(config["status"], "fail");
}

#[test]
fn doctor_evidence_warning_has_the_same_exit_when_piped() {
    let world = World::new();
    // A terminal of its own has only the world's bin on PATH: a git there,
    // as most machines have one, keeps doctor's `fork` check passing.
    world.git_on_path();
    world.configure(CONFIG);
    let person = world.as_a_person(&["doctor"]).finish();
    assert_eq!(person.code, 0, "{}", person.text());
    let text = person.text();
    // The same labels, in the same order, with the same words after them.
    let mut at = 0;
    for label in [ROLE_A, ROLE_B, KIND_MEDIUM, KIND_HIGH] {
        let found = text[at..]
            .find(&format!("▲  {label}"))
            .unwrap_or_else(|| panic!("{label} out of order or missing in:\n{text}"));
        at += found + label.len();
    }
    let flat: Vec<&str> = text
        .lines()
        .map(|line| line.trim_start_matches('│').trim())
        .collect();
    assert!(
        flat.join(" ").contains(
            "no rated or failed runs on record for this candidate in this list — \
             not enough evidence"
        ),
        "{text}"
    );
    let piped = world.with_stdin_elsewhere(&["doctor"]).finish();
    assert_eq!(piped.code, 0, "{}", piped.text());
    let json: Value = serde_json::from_str(piped.text().trim()).unwrap();
    assert_eq!(json["v"], 1);
    let labels: Vec<&str> = json["data"]["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| check["check"].as_str().unwrap())
        .filter(|label| label.ends_with(" evidence"))
        .collect();
    assert_eq!(labels, [ROLE_A, ROLE_B, KIND_MEDIUM, KIND_HIGH]);
}
