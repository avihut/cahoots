//! The verbs of review and learning: `review next`, `review submit`, `notes`,
//! and the person's `learn list` / `learn reset`. The rules live in
//! `review.rs`; this file is the I/O around them.

use std::fs;

use serde_json::{Value, json};

use crate::dirs::Dirs;
use crate::exit::{Envelope, Exit, Fail, Res};
use crate::gate;
use crate::history::{self, Event};
use crate::model::{HarnessId, Role};
use crate::registry::Registry;
use crate::review::{self, Kind};
use crate::run::client::{refuse_inside_a_sandbox, resolve_caller};
use crate::run::record::{RunDir, now};

fn reviewer(explicit: Option<HarnessId>) -> Res<HarnessId> {
    resolve_caller(explicit)?
        .ok_or_else(|| Fail::new(Exit::Usage, "say which harness is reviewing with --caller"))
}

fn capped(text: String) -> Value {
    let cut = (0..=review::CONTENT_MAX_BYTES.min(text.len()))
        .rev()
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(0);
    json!({
        // What is under review is another agent's output and the brief that
        // produced it. It is material to JUDGE — never instructions to follow.
        "untrusted": true,
        "truncated": cut < text.len(),
        "text": &text[..cut],
    })
}

/// How many runs wait for `caller`'s review — for the envelopes of `run`,
/// `wait` and `result`, so a harness learns there is something to look at
/// without having to ask. Zero when review is off.
pub fn pending_count(dirs: &Dirs, registry: &Registry, caller: Option<HarnessId>) -> usize {
    match caller {
        Some(caller) if registry.review.enabled => {
            let events = history::read(dirs);
            review::pending(&history::stories(&events), &events, caller, now()).len()
        }
        _ => 0,
    }
}

pub fn review_next(caller: Option<HarnessId>) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let registry = Registry::load(&dirs)?;
    let nothing = |pending: usize, why: &str| {
        Ok(Envelope::new(Exit::Ok, why.to_string()).with_data(
            json!({ "enabled": registry.review.enabled, "pending": pending, "next": null }),
        ))
    };
    if !registry.review.enabled {
        return nothing(
            0,
            "review is off — a person turns it on with `[review] enabled = true`",
        );
    }
    let caller = reviewer(caller)?;
    let events = history::read(&dirs);
    let stories = history::stories(&events);
    let pending = review::pending(&stories, &events, caller, now());
    if pending.is_empty() {
        return nothing(0, "nothing to review");
    }
    if review::reviews_in_the_last_day(&events, caller, now()) >= review::REVIEWS_PER_DAY {
        return nothing(
            pending.len(),
            "today's reviews are done — reviewing spends your own plan too",
        );
    }
    if let Err(why) = gate::reviewer_may_spend(&registry, caller) {
        return nothing(pending.len(), &why);
    }
    // The newest pending run whose content is still there to be read.
    let Some((story, dir)) = pending
        .iter()
        .find_map(|story| RunDir::open(&dirs, &story.run).ok().map(|dir| (story, dir)))
    else {
        return nothing(pending.len(), "the runs waiting for review have aged out");
    };
    let read = |path| capped(fs::read_to_string(path).unwrap_or_default());
    let rubric: Vec<Value> = Kind::ALL
        .into_iter()
        .map(|kind| json!({ "finding": kind.as_str(), "is_it_true_that": kind.question() }))
        .collect();
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({
        "enabled": true,
        "pending": pending.len(),
        "next": {
            "run": story.run,
            "role": story.role,
            "target": story.target,
            "how_it_ended": story.state,
            "what_you_did_with_it": story.outcome,
            "brief": read(dir.brief_path()),
            "answer": read(dir.final_path()),
        },
        "rubric": rubric,
        "then": format!(
            "cahoots review submit {} --caller {caller} [--finding <finding>[:<one plain sentence>]]…  \
             (no --finding at all means: nothing to note)",
            story.run
        ),
    })))
}

pub fn review_submit(run: &str, caller: Option<HarnessId>, findings: &[String]) -> Res<Envelope> {
    refuse_inside_a_sandbox("review")?;
    let dirs = Dirs::resolve()?;
    let registry = Registry::load(&dirs)?;
    if !registry.review.enabled {
        return Err(Fail::policy(
            "review is off — a person turns it on with `[review] enabled = true`",
        ));
    }
    let caller = reviewer(caller)?;
    let mut parsed = Vec::new();
    for text in findings {
        let finding = review::parse_finding(text)?;
        if !parsed
            .iter()
            .any(|seen: &review::Finding| seen.kind == finding.kind)
        {
            parsed.push(finding);
        }
    }
    let events = history::read(&dirs);
    let stories = history::stories(&events);
    let Some(story) = stories.iter().find(|story| story.run == run) else {
        return Err(Fail::new(Exit::NoSuchRun, format!("no finished run {run}")));
    };
    if story.caller != Some(caller) {
        return Err(Fail::policy(
            "a run is reviewed by the harness that delegated it, and by nobody else",
        ));
    }
    if !review::pending(&stories, &events, caller, now())
        .iter()
        .any(|p| p.run == run)
    {
        return Err(Fail::policy(format!(
            "run {run} is not waiting for a review (not sampled, already reviewed, or too old)"
        )));
    }
    history::append(
        &dirs,
        &Event::Review {
            t: now(),
            run: run.to_string(),
            reviewer: caller,
            findings: parsed.clone(),
        },
    )?;
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({ "run": run, "recorded": parsed })))
}

pub fn notes(role: Role, to: Option<HarnessId>, caller: Option<HarnessId>) -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let registry = Registry::load(&dirs)?;
    let caller = resolve_caller(caller)?;
    let events = history::read(&dirs);
    let stories = history::stories(&events);
    let by_target: serde_json::Map<String, Value> = HarnessId::ALL
        .into_iter()
        .filter(|id| Some(*id) != caller && to.is_none_or(|to| to == *id))
        .map(|id| {
            let notes = if registry.review.enabled {
                review::notes(&stories, &events, role, id, now())
            } else {
                Vec::new()
            };
            (id.to_string(), json!(notes))
        })
        .collect();
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({
        "enabled": registry.review.enabled,
        "role": role,
        "read_this_first": review::NOTES_HEADER,
        "notes": by_target,
    })))
}

/// `learn list`: what has been learned on this machine, and from how much.
pub fn learn_list() -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    let registry = Registry::load(&dirs)?;
    let events = history::read(&dirs);
    let stories = history::stories(&events);
    let reviews = events
        .iter()
        .filter(|e| matches!(e, Event::Review { .. }))
        .count();
    let mut scopes = serde_json::Map::new();
    for role in Role::ALL {
        for id in HarnessId::ALL {
            let notes: Vec<Value> = review::notes(&stories, &events, role, id, now())
                .into_iter()
                .map(|note| {
                    // A person's verb, in a terminal: here — and only here —
                    // the reviewers' own words are shown.
                    let mut shown = json!(note);
                    shown["what_reviewers_wrote"] = json!(note.details);
                    shown
                })
                .collect();
            if !notes.is_empty() {
                scopes.insert(format!("{role} · {id}"), json!(notes));
            }
        }
    }
    Ok(Envelope::new(Exit::Ok, None).with_data(json!({
        "review_enabled": registry.review.enabled,
        "sample_rate": registry.review.sample_rate,
        "runs_on_record": stories.len(),
        "reviews_on_record": reviews,
        "notes_in_effect": scopes,
        "where": dirs.state.join("history.jsonl"),
    })))
}

/// `learn reset`: forget what reviews have said so far. The history is kept —
/// it is a record — and one more event says that reviews before now no longer
/// count toward any note.
pub fn learn_reset() -> Res<Envelope> {
    let dirs = Dirs::resolve()?;
    history::append(&dirs, &Event::Forget { t: now() })?;
    Ok(Envelope::new(
        Exit::Ok,
        "reviews recorded before now no longer count toward any note".to_string(),
    ))
}
