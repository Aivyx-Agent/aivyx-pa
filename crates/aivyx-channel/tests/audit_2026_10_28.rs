//! Audit 2026-10-28 (P2) regression test.
//!
//! `[loop] gate_command` (e.g. `cargo test`) runs in the directory the loop's
//! confined agent writes to (`working_dir`). What the agent left there
//! (`build.rs`, a test, `./gate.sh`, `.cargo/config.toml`) is executed by the
//! gate, so the gate runs confined with a cleared environment and cannot read
//! `AIVYX_PA_PASSPHRASE` or a provider key.
use std::time::Duration;

use aivyx_channel::loop_gate::{GateOutcome, GateRunner, ShellGateRunner};

#[tokio::test]
async fn gate_runs_agent_written_code_without_the_daemons_secrets() {
    let work = std::env::temp_dir().join(format!("aivyx-gate-probe-{}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let loot = work.join("loot.txt");
    // What a confined agent could leave behind (it may write its own folder).
    std::fs::write(
        work.join("gate.sh"),
        format!("echo \"$AIVYX_PA_PASSPHRASE\" > {}\nexit 0\n", loot.display()),
    )
    .unwrap();
    // SAFETY: single test in this binary.
    unsafe { std::env::set_var("AIVYX_PA_PASSPHRASE", "daemon-master-passphrase-canary") };
    let gate = ShellGateRunner::new("sh ./gate.sh".into(), Some(work.clone()), Duration::from_secs(10));
    assert_eq!(gate.run().await, GateOutcome::Passed);
    let leaked = std::fs::read_to_string(&loot).unwrap_or_default();
    assert!(
        !leaked.contains("daemon-master-passphrase-canary"),
        "the gate (agent-written code) saw the daemon's passphrase: {leaked:?}"
    );
}
