//! Emits `cfg(cahoots_dev_build)` only when the build's environment says so.
//!
//! The cfg unlocks the `CAHOOTS_*_DIR` overrides the test suite needs to stay
//! out of the real state directories. It is opt-IN by an explicit variable —
//! never inferred from "this looks like a checkout", which `cargo install
//! --git` would satisfy — so a binary anyone installs ignores those variables
//! entirely. `cahoots --version` says when they are honoured.

fn main() {
    println!("cargo:rerun-if-env-changed=CAHOOTS_DEV_BUILD");
    if std::env::var("CAHOOTS_DEV_BUILD").as_deref() == Ok("1") {
        println!("cargo:rustc-cfg=cahoots_dev_build");
    }
}
