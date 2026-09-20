//! The ONLY module that reads the process environment (hard rule 4;
//! `scripts/guard.sh` holds it). An agent controls the environment of the
//! commands it runs, so every read here is either advisory, or refused in
//! favour of something the agent cannot set — see `dirs.rs` for `HOME`.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::model::HarnessId;

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Which cahoots directory an override names.
#[derive(Debug, Clone, Copy)]
pub enum DirKind {
    Config,
    State,
}

/// `CAHOOTS_CONFIG_DIR` / `CAHOOTS_STATE_DIR` — honoured ONLY by a dev build
/// (`build.rs`). In a build anyone installs this is a constant `None`: the
/// variables are not even read.
pub fn dev_dir_override(kind: DirKind) -> Option<PathBuf> {
    if !cfg!(cahoots_dev_build) {
        return None;
    }
    let name = match kind {
        DirKind::Config => "CAHOOTS_CONFIG_DIR",
        DirKind::State => "CAHOOTS_STATE_DIR",
    };
    var(name).map(PathBuf::from)
}

/// Whether directory overrides are compiled in at all.
pub const fn dev_overrides_honoured() -> bool {
    cfg!(cahoots_dev_build)
}

/// Every harness whose marker is in the environment. More than one is normal
/// when harnesses nest (docs/SPIKE.md S1), which is why the skill passes
/// `--caller` and this is only the fallback.
pub fn detected_callers() -> Vec<HarnessId> {
    let mut callers = Vec::new();
    if var("CLAUDECODE").as_deref() == Some("1") {
        callers.push(HarnessId::Claude);
    }
    if var("CODEX_THREAD_ID").is_some() {
        callers.push(HarnessId::Codex);
    }
    callers
}

/// True inside Codex's seatbelt/landlock sandbox. `CODEX_SANDBOX` is the
/// signal; `CODEX_SANDBOX_NETWORK_DISABLED` is set even for a command a rule
/// let out of the sandbox, so it says nothing (docs/SPIKE.md S1).
pub fn in_codex_sandbox() -> bool {
    var("CODEX_SANDBOX").is_some()
}

/// How many cahoots runs deep this process already is. Advisory: a caller can
/// scrub it, so the slots are the real bound on recursion.
pub fn depth() -> u32 {
    var("CAHOOTS_DEPTH")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

pub fn path_var() -> Option<OsString> {
    std::env::var_os("PATH").filter(|value| !value.is_empty())
}

pub fn tmpdir() -> Option<PathBuf> {
    var("TMPDIR").map(PathBuf::from)
}

/// The variables a callee inherits, by allowlist. Everything else — the
/// caller's harness markers, tokens, proxies, `LD_PRELOAD` — is dropped.
/// `HOME` is absent on purpose: the spawner sets it from the passwd database.
pub fn callee_passthrough(harness: HarnessId) -> Vec<(OsString, OsString)> {
    const ALWAYS: [&str; 8] = [
        "PATH", "USER", "LOGNAME", "LANG", "TERM", "TMPDIR", "SHELL", "TZ",
    ];
    let own_home: &[&str] = match harness {
        HarnessId::Claude => &["CLAUDE_CONFIG_DIR"],
        HarnessId::Codex => &["CODEX_HOME"],
    };
    std::env::vars_os()
        .filter(|(name, value)| {
            let name = name.to_string_lossy();
            !value.is_empty()
                && (ALWAYS.contains(&name.as_ref())
                    || own_home.contains(&name.as_ref())
                    || name.starts_with("LC_"))
        })
        .collect()
}

/// The vendor API key for a harness — passed on ONLY when that harness is
/// configured `billing = "api"`. An inherited key silently moves a callee
/// onto per-token billing that no meter sees.
pub fn api_key(harness: HarnessId) -> Option<(OsString, OsString)> {
    let name = match harness {
        HarnessId::Claude => "ANTHROPIC_API_KEY",
        HarnessId::Codex => "OPENAI_API_KEY",
    };
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| (OsString::from(name), value))
}
