//! `aivyx-vision` — the aivyx-pa tool process exposing
//! `vision.generate_svg` (Milestone 1) and
//! `vision.generate_image`/`vision.generate_3d` (Milestone 2 Pass A),
//! backed by the standalone `aivyx-vision-svg`/`aivyx-vision-core`/
//! `aivyx-vision-mold` crates. See `README.md` and
//! `docs/superpowers/specs/2026-09-18-vision-image-3d-adoption-design.md`
//! (this repo) and `aivyx-ecosystem/docs/superpowers/specs/
//! 2026-09-18-aivyx-vision-v1-design.md` for the full design.

pub mod config;
pub mod generation_tools;
pub mod text_completer;
pub mod tools;
