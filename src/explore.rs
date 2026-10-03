//! Exploration: a share of new runs tries the NEXT listed candidate first, so
//! a second model or effort on the same harness can earn evidence. This is the
//! pure part — the draw and the one adjacent swap. Admission is not here:
//! every candidate still walks `pick::eligible`, in the order this leaves.
//!
//! The draw is a property of the run's own id and the share in effect, like
//! the review sample (`history::is_sampled`): nobody supplies a seed, and the
//! id is generated before any candidate is looked at.

use crate::history::fnv1a64;
use crate::model::Candidate;

/// Keeps this draw apart from every other use of the id's hash. Frozen: a
/// change re-draws every run id, past and future.
const PREFIX: &[u8] = b"cahoots-explore-v1\0";

/// Buckets in a draw: the same resolution as the review sample.
const BUCKETS: u64 = 10_000;

/// Whether the run with this id is drawn for exploration at `share`. Strict:
/// a bucket at exactly `share * 10_000` is not selected, so `0.0` selects
/// nothing and `1.0` everything.
pub fn drawn(run_id: &str, share: f64) -> bool {
    let mut bytes = PREFIX.to_vec();
    bytes.extend_from_slice(run_id.as_bytes());
    ((fnv1a64(&bytes) % BUCKETS) as f64) < share * BUCKETS as f64
}

/// Whether a list that is left after the caller and disabled targets are out
/// can explore at all: at least two entries, the first two not the same
/// candidate (harness, model AND effort).
pub fn can_explore(list: &[Candidate]) -> bool {
    matches!(list, [first, second, ..] if first != second)
}

/// The order a list is tried in, as indices into it: its own order, or with
/// the first two swapped. Never more than that one swap; the tail stays put.
pub fn order(len: usize, promote: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    if promote && len >= 2 {
        order.swap(0, 1);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Effort, HarnessId, ModelName};

    fn candidate(harness: HarnessId, model: &str, effort: Effort) -> Candidate {
        Candidate {
            harness,
            model: ModelName::try_from(model.to_string()).unwrap(),
            effort,
        }
    }

    fn ids(count: u32) -> Vec<String> {
        (0..count)
            .map(|n| format!("0198c0de-0000-7000-8000-{n:012}"))
            .collect()
    }

    fn bucket(run_id: &str) -> u64 {
        let mut bytes = PREFIX.to_vec();
        bytes.extend_from_slice(run_id.as_bytes());
        fnv1a64(&bytes) % BUCKETS
    }

    #[test]
    fn the_exploration_draw_is_frozen() {
        for (id, hash, bucket_of) in [
            (
                "0198c0de-0000-7000-8000-000000000000",
                0xecf4_e00e_ae44_c9f9,
                665,
            ),
            (
                "0198c0de-0000-7000-8000-000000000001",
                0xecf4_df0e_ae44_c846,
                2454,
            ),
        ] {
            let mut bytes = b"cahoots-explore-v1\0".to_vec();
            bytes.extend_from_slice(id.as_bytes());
            assert_eq!(fnv1a64(&bytes), hash, "{id}");
            assert_eq!(bucket(id), bucket_of, "{id}");
            // Repeated calls agree.
            assert_eq!(drawn(id, 0.5), drawn(id, 0.5));
            // The strict comparison: a share that ends exactly at the bucket
            // does not select it; one step past it does.
            let at = bucket_of as f64 / BUCKETS as f64;
            assert!(!drawn(id, at), "{id} at exactly its bucket");
            assert!(drawn(id, at + 1.0 / BUCKETS as f64), "{id} one bucket past");
            // The endpoints.
            assert!(!drawn(id, 0.0));
            assert!(drawn(id, 1.0));
        }
        // The prefix keeps it apart from the review sample.
        let id = "0198c0de-0000-7000-8000-000000000000";
        assert_ne!(fnv1a64(id.as_bytes()) % BUCKETS, bucket(id));
    }

    #[test]
    fn the_exploration_draw_tracks_the_share() {
        let ids = ids(4000);
        let picked = |share| ids.iter().filter(|id| drawn(id, share)).count();
        assert_eq!(picked(0.0), 0);
        assert_eq!(picked(1.0), ids.len());
        let fifth = picked(0.2);
        assert!((600..1000).contains(&fifth), "0.2 of 4000 selected {fifth}");
        // Raising the share only ever ADDS runs.
        for id in &ids {
            assert!(!drawn(id, 0.2) || drawn(id, 0.5), "{id}");
        }
        assert!(picked(0.5) > fifth);
    }

    #[test]
    fn exploration_is_at_most_one_adjacent_swap() {
        use Effort::{High, Low, Medium};
        use HarnessId::{Claude, Codex};
        let a = candidate(Codex, "m", High);
        let b = candidate(Codex, "m", Medium);
        let c = candidate(Claude, "n", Low);
        assert!(can_explore(&[a.clone(), b.clone(), c.clone()]));
        assert!(can_explore(&[a.clone(), b.clone()]));
        // Different efforts of one model are distinct; nothing else is needed.
        assert!(can_explore(&[a.clone(), candidate(Codex, "m", Low)]));
        assert!(can_explore(&[a.clone(), candidate(Codex, "other", High)]));
        assert!(can_explore(&[a.clone(), candidate(Claude, "m", High)]));
        // Suppressed: one entry, none, and an identical second entry — and the
        // search does not go on past it.
        assert!(!can_explore(std::slice::from_ref(&a)));
        assert!(!can_explore(&[]));
        assert!(!can_explore(&[a.clone(), a.clone(), c.clone()]));
        assert!(!can_explore(&[a.clone(), a.clone(), b.clone()]));

        assert_eq!(order(0, true), Vec::<usize>::new());
        assert_eq!(order(1, true), [0]);
        assert_eq!(order(2, false), [0, 1]);
        assert_eq!(order(2, true), [1, 0]);
        assert_eq!(order(5, false), [0, 1, 2, 3, 4]);
        assert_eq!(order(5, true), [1, 0, 2, 3, 4]);
    }
}
