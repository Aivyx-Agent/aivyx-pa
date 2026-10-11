# Vendored crates

## `serde-saphyr` 0.0.16 (MIT OR Apache-2.0)

Upstream: https://github.com/bourumir-wyngs/serde-saphyr — unchanged except
for one line in `Cargo.toml`: the `regex` requirement `>= 1.0, < 1.13` is
relaxed to `>= 1.0, < 2`.

Why: `mistralrs-core` 0.8.1 (the in-process mistral.rs engine behind
`aivyx-llm`'s `mistralrs` feature) pins `serde-saphyr` 0.0.16, whose `< 1.13`
cap conflicts with `matrix-sdk` 0.19 (the Matrix channel), which needs
`regex` 1.13.1 or newer. The cap is conservative pinning, not a known
incompatibility: the crate uses only `regex::Regex::new` / `is_match`, whose
API is unchanged in 1.13.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml`. Remove
this directory and the patch once `mistralrs-core` moves to a `serde-saphyr`
release without the cap.
