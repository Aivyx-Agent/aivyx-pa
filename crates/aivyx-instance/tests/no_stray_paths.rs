//! Fails if any non-test source outside aivyx-instance builds an aivyx-pa
//! path or per-instance name itself, so the instance namespace can't be
//! bypassed by a new call site. A line that legitimately isn't per-instance
//! carries an `// instance-paths: ok — <reason>` marker.
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    ".join(\".aivyx-pa\")",
    ".join(\"aivyx-pa\")",
    "\"aivyx-pa-sandbox\"",
    "\"aivyx-pa-daemon.service\"",
    "\"com.aivyx-pa.daemon",
    ".local/share/aivyx-pa",
    ".config/aivyx-pa",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if ["target", "tests", "aivyx-instance", "verticals-private"].contains(&name.as_str()) { continue; }
            rust_files(&p, out);
        } else if name.ends_with(".rs") && name != "tests.rs" {
            out.push(p);
        }
    }
}

#[test]
fn no_crate_builds_aivyx_pa_paths_itself() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    let mut hits = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        // Only check code before the first `#[cfg(test)]` module.
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (i, line) in code.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") || line.contains("instance-paths: ok") { continue; }
            for pat in FORBIDDEN {
                if line.contains(pat) { hits.push(format!("{}:{}: {}", f.display(), i + 1, t)); }
            }
        }
    }
    assert!(hits.is_empty(), "build these through aivyx_instance::InstancePaths:\n{}", hits.join("\n"));
}
