//! The binary's outer contract: exit codes and the JSON envelope. The test
//! harness gives the child no terminal on stdin, which is exactly an agent's
//! situation.

use assert_cmd::Command;
use predicates::prelude::*;

fn cahoots() -> Command {
    Command::cargo_bin("cahoots").unwrap()
}

#[test]
fn version_names_the_crate_version() {
    cahoots()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with(concat!(
            "cahoots ",
            env!("CARGO_PKG_VERSION")
        )));
}

#[test]
fn a_bad_command_line_is_exit_2_with_an_envelope_like_any_other_exit() {
    for argv in [
        vec!["conspire"],
        vec![],
        vec!["run", "--role", "deploy", "--brief", "b.md"],
        vec!["run", "--role", "review", "--brief", "b.md", "--ungated"],
    ] {
        let output = cahoots().args(&argv).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{argv:?}");
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{argv:?}: no envelope on stdout ({error})"));
        assert_eq!(envelope["class"], "usage_error");
        assert_eq!(envelope["retry"], "never");
        assert!(!envelope["message"].as_str().unwrap().is_empty());
    }
    // …while asking for help or the version is an answer, not an error.
    cahoots().arg("--help").assert().success();
}

#[test]
fn a_human_verb_without_a_terminal_is_refused_by_policy() {
    for verb in [
        vec!["install"],
        vec!["uninstall"],
        vec!["enable", "codex"],
        vec!["learn"],
        vec!["registry"],
    ] {
        cahoots()
            .args(verb)
            .assert()
            .code(33)
            .stdout(predicate::str::contains(r#""class":"refused_by_policy""#))
            .stdout(predicate::str::contains(r#""retry":"never""#));
    }
}

#[test]
fn every_exit_prints_one_json_envelope() {
    let output = cahoots().arg("status").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let envelope: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(envelope["v"], 1);
    assert_eq!(
        envelope["code"].as_i64(),
        output.status.code().map(i64::from)
    );
}

#[test]
fn exit_codes_prints_the_taxonomy() {
    let output = cahoots().arg("exit-codes").output().unwrap();
    assert!(output.status.success());
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let codes = envelope["data"].as_array().unwrap();
    assert!(
        codes
            .iter()
            .any(|entry| entry["code"] == 24 && entry["retry"] == "after_reset")
    );
}
