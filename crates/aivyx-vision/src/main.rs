//! `aivyx-vision` tool-process binary entry point.
//!
//! Scaffold only: this task adds the crate, its LLM provider config
//! (`aivyx_vision::config`), and the pinned `aivyx-vision-svg` dependency.
//! Wiring the actual tool-process IPC loop (`aivyx-tool`) and the
//! `vision.generate_svg` handler backed by `aivyx-llm`/`aivyx-vision-svg`
//! is later work — see `aivyx-ecosystem/docs/superpowers/specs/
//! 2026-09-18-aivyx-vision-v1-design.md`.

fn main() {
    todo!("aivyx-vision tool-process IPC loop -- wired in a later task");
}
