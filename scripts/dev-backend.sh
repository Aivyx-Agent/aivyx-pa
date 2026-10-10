# Shared by dev-run.sh and dev-verify.sh (sourced, not run).
#
# dev_backend_defaults PROVIDER URL MODEL
#   Fills in DEV_URL and DEV_MODEL for the provider when they're empty.
# dev_backend_preflight TAG
#   Checks the backend answers (and, for Ollama, that the model is
#   pulled); prints a fix and returns 1 if not. TAG prefixes messages.
# dev_home_env DEV_DIR
#   Prints `NAME=value` lines that point HOME and the XDG directories
#   inside DEV_DIR, for `env`. The binary derives ~/.aivyx-pa,
#   ~/.local/share/aivyx-pa and ~/.config/aivyx-pa from them; without
#   this a dev run writes into the operator's real home. Only for the
#   binary: cargo needs the real HOME to find ~/.cargo.

dev_backend_defaults() {
    case "$DEV_PROVIDER" in
        ollama)
            DEV_URL="${DEV_URL:-http://localhost:11434}"
            DEV_MODEL="${DEV_MODEL:-llama3.1}"
            ;;
        llamacpp)
            DEV_URL="${DEV_URL:-http://127.0.0.1:8080}"
            ;;
        *)
            echo "unknown --provider '$DEV_PROVIDER' (ollama or llamacpp)" >&2
            return 2
            ;;
    esac
}

dev_backend_preflight() {
    local tag="$1" body
    case "$DEV_PROVIDER" in
        ollama)
            echo "$tag: checking Ollama at $DEV_URL"
            if ! body="$(curl -fsS --max-time 4 "$DEV_URL/api/tags" 2>/dev/null)"; then
                echo "$tag: ERROR — Ollama is not reachable at $DEV_URL" >&2
                echo "$tag:   start it with:  ollama serve   (or --url <url>)" >&2
                return 1
            fi
            # /api/tags names models e.g. "llama3.1:latest"; a bare
            # "llama3.1" must match the "llama3.1:" prefix or exactly.
            if ! grep -qE "\"name\"[[:space:]]*:[[:space:]]*\"${DEV_MODEL}(:|\")" <<<"$body"; then
                echo "$tag: ERROR — model '$DEV_MODEL' is not pulled on this Ollama instance" >&2
                echo "$tag:   pull it with:  ollama pull $DEV_MODEL   (or --model <name>)" >&2
                return 1
            fi
            ;;
        llamacpp)
            echo "$tag: checking llama-server at $DEV_URL"
            if ! curl -fsS --max-time 4 "$DEV_URL/health" >/dev/null 2>&1; then
                echo "$tag: ERROR — llama-server is not ready at $DEV_URL" >&2
                echo "$tag:   start it with:  llama-server -m <model.gguf> --jinja --port 8080   (or --url <url>)" >&2
                return 1
            fi
            # llama-server serves one model under whatever name; use the
            # one it reports unless --model named another.
            if [[ -z "$DEV_MODEL" ]]; then
                body="$(curl -fsS --max-time 4 "$DEV_URL/v1/models" 2>/dev/null)"
                DEV_MODEL="$(grep -oE '"id"[[:space:]]*:[[:space:]]*"[^"]+"' <<<"$body" \
                    | head -1 | sed -E 's/.*"([^"]+)"$/\1/')"
                DEV_MODEL="${DEV_MODEL:-local}"
            fi
            ;;
    esac
}

dev_home_env() {
    local home="$1/home"
    mkdir -p "$home"
    printf '%s\n' \
        "HOME=$home" \
        "XDG_CONFIG_HOME=$home/.config" \
        "XDG_DATA_HOME=$home/.local/share" \
        "XDG_STATE_HOME=$home/.local/state" \
        "XDG_CACHE_HOME=$home/.cache"
}
