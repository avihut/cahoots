//! Identity projection for run and review views. Private attribution stays whole.

use serde_json::{Value, json};

use crate::model::Candidate;

pub struct Identity {
    pub blind: bool,
    pub target: Value,
}

pub fn project(stored_blind: bool, has_outcome: bool, target: &Candidate) -> Identity {
    let blind = stored_blind && !has_outcome;
    let mut projected = json!(target);
    if blind && let Some(object) = projected.as_object_mut() {
        object.remove("model");
        object.remove("effort");
    }
    Identity {
        blind,
        target: projected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_outcome_reveals_a_stored_blind_identity() {
        let candidate: Candidate = serde_json::from_value(json!({
            "harness": "codex", "model": "m", "effort": "high"
        }))
        .unwrap();
        for stored in [false, true] {
            for outcome in [false, true] {
                let identity = project(stored, outcome, &candidate);
                assert_eq!(identity.blind, stored && !outcome);
                assert_eq!(
                    identity.target,
                    if identity.blind {
                        json!({"harness": "codex"})
                    } else {
                        json!(candidate)
                    }
                );
            }
        }
        assert_eq!(candidate.model.as_str(), "m");
    }
}
