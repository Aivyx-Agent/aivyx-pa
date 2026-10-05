//! Named instances through the real `aivyx-pa` binary, in a throwaway
//! HOME: each instance resolves its own socket, `instances list` sees
//! them, and an invalid name is refused before anything runs.

use std::path::Path;
use std::process::{Command, Output};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aivyx-pa"))
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", home.join("run"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .current_dir(home)
        .output()
        .expect("run aivyx-pa")
}

fn text(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

#[test]
fn each_instance_has_its_own_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    let default = text(&run(h, &["daemon", "status"]));
    let research = text(&run(h, &["--instance", "research", "daemon", "status"]));
    let env_research = {
        let o = Command::new(env!("CARGO_BIN_EXE_aivyx-pa"))
            .args(["daemon", "status"])
            .env_clear()
            .env("HOME", h)
            .env("XDG_RUNTIME_DIR", h.join("run"))
            .env("AIVYX_PA_INSTANCE", "research")
            .current_dir(h)
            .output()
            .unwrap();
        text(&o)
    };
    let default_sock = h.join("run/aivyx-pa/daemon.sock");
    let research_sock = h.join("run/aivyx-pa/instances/research/daemon.sock");
    assert!(default.contains(&default_sock.display().to_string()), "{default}");
    assert!(research.contains(&research_sock.display().to_string()), "{research}");
    assert!(env_research.contains(&research_sock.display().to_string()), "{env_research}");
}

#[test]
fn instances_list_sees_default_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    std::fs::create_dir_all(h.join(".config/aivyx-pa/instances/research")).unwrap();
    std::fs::write(
        h.join(".config/aivyx-pa/instances/research/aivyx-pa.toml"),
        "[daemon]\nweb_ui_port = 7844\n",
    )
    .unwrap();
    std::fs::write(h.join(".config/aivyx-pa/aivyx-pa.toml"), "").unwrap();
    let out = run(h, &["instances", "list"]);
    let t = text(&out);
    assert!(out.status.success(), "{t}");
    assert!(t.lines().any(|l| l.starts_with("default ") && l.contains("7843")), "{t}");
    assert!(t.lines().any(|l| l.starts_with("research ") && l.contains("7844")), "{t}");
}

#[test]
fn invalid_instance_names_are_refused_up_front() {
    let tmp = tempfile::tempdir().unwrap();
    for args in [&["--instance", "Bad_Name", "daemon", "status"][..], &["--instance"][..]] {
        let out = run(tmp.path(), args);
        assert!(!out.status.success(), "{args:?}: {}", text(&out));
    }
    // Nothing was created for the rejected instance.
    assert!(!tmp.path().join(".config/aivyx-pa/instances").exists());
}
