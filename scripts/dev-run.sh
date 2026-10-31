#!/usr/bin/env bash
#
# aivyx-pa local dev launcher — Phase 99 Task 1.
#
# Phase 99 keeps all builds local while repo infrastructure is still
# being decided (no CI, no remote runners). This script is the
# local-testing entry point: it builds the `aivyx-pa` binary and
# launches it against a **fully local Ollama backend** with a
# disposable, gitignored state directory — so a `run Aivyx locally`
# loop is one command and leaves no footprint outside `.dev-run/`.
#
# What it does:
#   1. Preflights the Ollama server (reachable + requested model
#      pulled) so a misconfigured backend fails fast with an
#      actionable message instead of mid-session.
#   2. Builds `aivyx-pa` locally (`cargo build --bin aivyx-pa`).
#   3. Execs the binary with the Ollama provider selected and every
#      path pinned under `.dev-run/` — sandbox FS root, encrypted
#      store, and a throwaway dev passphrase.
#
# Why Ollama: no API key, no network egress, no per-run cost — the
# truest fit for a local testing loop. The Anthropic provider is
# deliberately out of scope here (Phase 99 decision).
#
# Why a fixed dev passphrase: the redb store is encrypted, so the
# passphrase must be stable across runs or each launch would fail to
# reopen the store. `.dev-run/` is disposable scratch state, never
# real data, so a hardcoded throwaway is correct here — do NOT reuse
# this pattern for a real install.
#
# Usage:
#   ./scripts/dev-run.sh [options] [-- <args passed to aivyx-pa>]
#
# Options:
#   --provider <p>      ollama or llamacpp       (default: ollama)
#   --model <name>      Model to use             (default: llama3.1 on Ollama;
#                                                 llama-server's own model)
#   --url <url>         Backend base URL         (default: http://localhost:11434
#                                                 for Ollama, http://127.0.0.1:8080
#                                                 for llama-server)
#   --ollama-url <url>  Same as --url
#   --release           Build with --release     (default: debug)
#   --reset             Wipe .dev-run/ first for a clean store/sandbox
#   --verify            Run the scripted verification pass instead of
#                       an interactive session (delegates to
#                       scripts/dev-verify.sh)
#   -h, --help          Show this help and exit
#
# Anything after `--` is forwarded verbatim to the binary, e.g.:
#   ./scripts/dev-run.sh -- --verify-only      # forensic audit walk
#   ./scripts/dev-run.sh -- --role coder       # start in a named role
#
# Examples:
#   ./scripts/dev-run.sh                       # interactive session
#   ./scripts/dev-run.sh --model llama3.2 --reset
#   ./scripts/dev-run.sh --release
#   ./scripts/dev-run.sh --provider llamacpp   # llama-server on :8080
#
# HOME and the XDG directories point inside .dev-run/ for the binary, so
# nothing it derives from them (~/.aivyx-pa, ~/.local/share/aivyx-pa)
# lands in your real home.

set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
DEV_DIR="$REPO_ROOT/.dev-run"

# --- defaults (each overridable via the matching flag) ---------------
DEV_PROVIDER="${AIVYX_PA_DEV_PROVIDER:-ollama}"
DEV_MODEL="${AIVYX_PA_MODEL:-}"
DEV_URL="${AIVYX_PA_OLLAMA_URL:-}"
# Dev-only throwaway. See header — never a real secret.
PASSPHRASE="${AIVYX_PA_DEV_PASSPHRASE:-aivyx-dev-throwaway}"
BUILD_PROFILE="debug"
DO_RESET=0
DO_VERIFY=0
PASSTHROUGH=()

usage() { sed -n '2,61p' "$0" | sed 's/^# \{0,1\}//'; }

# shellcheck source=dev-backend.sh
source "$REPO_ROOT/scripts/dev-backend.sh"

# --- argument parsing ------------------------------------------------
while [[ $# -gt 0 ]]; do
    case "$1" in
        --provider)   DEV_PROVIDER="${2:?--provider requires a value}"; shift 2 ;;
        --model)      DEV_MODEL="${2:?--model requires a value}"; shift 2 ;;
        --url|--ollama-url) DEV_URL="${2:?$1 requires a value}"; shift 2 ;;
        --release)    BUILD_PROFILE="release"; shift ;;
        --reset)      DO_RESET=1; shift ;;
        --verify)     DO_VERIFY=1; shift ;;
        -h|--help)    usage; exit 0 ;;
        --)           shift; PASSTHROUGH+=("$@"); break ;;
        *)            PASSTHROUGH+=("$1"); shift ;;
    esac
done

# --- verification mode: hand off to the scripted pass ---------------
# `--verify` is a thin alias so the verification battery is reachable
# from the same entry point as the interactive launcher. dev-verify.sh
# owns its own preflight, build, and clean-baseline handling.
dev_backend_defaults || exit 2
if [[ $DO_VERIFY -eq 1 ]]; then
    exec "$REPO_ROOT/scripts/dev-verify.sh" --provider "$DEV_PROVIDER" \
        --url "$DEV_URL" ${DEV_MODEL:+--model "$DEV_MODEL"}
fi

# --- preflight: backend reachable (and, on Ollama, the model pulled) -
dev_backend_preflight dev-run || exit 1

# --- disposable state directory -------------------------------------
if [[ $DO_RESET -eq 1 ]]; then
    echo "dev-run: --reset — wiping $DEV_DIR"
    rm -rf "$DEV_DIR"
fi
mkdir -p "$DEV_DIR/sandbox"

# --- build (local only — Phase 99) ----------------------------------
echo "dev-run: building aivyx-pa ($BUILD_PROFILE)"
if [[ "$BUILD_PROFILE" == "release" ]]; then
    cargo build --bin aivyx-pa --release
else
    cargo build --bin aivyx-pa
fi
BIN="$REPO_ROOT/target/$BUILD_PROFILE/aivyx-pa"

# --- launch ----------------------------------------------------------
echo "dev-run: launching"
echo "dev-run:   provider = $DEV_PROVIDER   model = $DEV_MODEL"
echo "dev-run:   backend  = $DEV_URL"
echo "dev-run:   home     = $DEV_DIR/home"
echo "dev-run:   sandbox  = $DEV_DIR/sandbox"
echo "dev-run:   store    = $DEV_DIR/store.redb"
echo "dev-run:   args     = ${PASSTHROUGH[*]:-(none)}"
echo

# CWD is .dev-run/. A config is never read from the working directory;
# set AIVYX_PA_CONFIG_PATH (or put one under the dev HOME's
# .config/aivyx-pa/) if a dev role config is wanted.
cd "$DEV_DIR"
mapfile -t HOME_ENV < <(dev_home_env "$DEV_DIR")
exec env \
    "${HOME_ENV[@]}" \
    AIVYX_PA_PROVIDER="$DEV_PROVIDER" \
    AIVYX_PA_MODEL="$DEV_MODEL" \
    AIVYX_PA_OPENAI_BASE_URL="$DEV_URL" \
    AIVYX_PA_FS_ROOT="$DEV_DIR/sandbox" \
    AIVYX_PA_STORAGE_PATH="$DEV_DIR/store.redb" \
    AIVYX_PA_PASSPHRASE="$PASSPHRASE" \
    "$BIN" ${PASSTHROUGH[@]+"${PASSTHROUGH[@]}"}
