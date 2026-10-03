//! `cancel_settled` tolerates a cancel's 51 only when it says the run is
//! still unfinished. Controlled answers stand in for `cancel`'s.
mod common;

use common::{Answer, cancel_is_pending};
use serde_json::json;

fn answer(code: i32, state: &str) -> Answer {
    Answer {
        code,
        json: json!({"code": code, "data": {"state": state}}),
    }
}

#[test]
fn a_51_for_an_unfinished_run_is_a_cancel_still_going() {
    assert!(cancel_is_pending(&answer(51, "running")));
    assert!(cancel_is_pending(&answer(51, "starting")));
}

#[test]
#[should_panic(expected = "not unfinished")]
fn a_51_for_a_cancelled_run_is_a_wrong_answer() {
    cancel_is_pending(&answer(51, "cancelled"));
}

#[test]
#[should_panic(expected = "not unfinished")]
fn a_51_for_a_crashed_run_is_a_wrong_answer() {
    cancel_is_pending(&answer(51, "crashed"));
}

#[test]
fn any_other_code_is_left_to_the_caller() {
    for (code, state) in [
        (42, "cancelled"),
        (42, "running"),
        (0, "done"),
        (40, "failed"),
    ] {
        assert!(!cancel_is_pending(&answer(code, state)));
    }
}
