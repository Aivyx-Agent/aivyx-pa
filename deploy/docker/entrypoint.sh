#!/bin/sh
# Aivyx PA container entrypoint (Chapter Harbor, HB.1).
#
# Bridges a Docker *secret* (mounted as a file) to the AIVYX_PA_PASSPHRASE env
# var the daemon reads to unlock the encrypted store — so the passphrase never
# has to live in plain `environment:` (visible in `docker inspect`). Honors the
# conventional `_FILE` indirection.
set -eu

# If AIVYX_PA_PASSPHRASE isn't already set, source it from a file:
#   1. $AIVYX_PA_PASSPHRASE_FILE if set, else
#   2. the default Docker secret path.
if [ -z "${AIVYX_PA_PASSPHRASE:-}" ]; then
    _pass_file="${AIVYX_PA_PASSPHRASE_FILE:-/run/secrets/aivyx_pa_passphrase}"
    if [ -f "$_pass_file" ]; then
        AIVYX_PA_PASSPHRASE="$(cat "$_pass_file")"
        export AIVYX_PA_PASSPHRASE
    fi
fi

if [ -z "${AIVYX_PA_PASSPHRASE:-}" ]; then
    echo "aivyx-pa (entrypoint): no passphrase — set the aivyx_pa_passphrase secret" >&2
    echo "  (or AIVYX_PA_PASSPHRASE_FILE / AIVYX_PA_PASSPHRASE). See docs/DOCKER.md." >&2
    exit 2
fi

# The config the daemon reads (the Dockerfile sets AIVYX_PA_CONFIG_PATH; pa
# never reads one from its working directory). First boot seeds it with the
# baked appliance config unless the operator mounted their own (or a prior run
# already wrote one).
_cfg="${AIVYX_PA_CONFIG_PATH:-/root/.aivyx-pa/aivyx-pa.toml}"
export AIVYX_PA_CONFIG_PATH="$_cfg"
if [ ! -f "$_cfg" ]; then
    mkdir -p "$(dirname "$_cfg")"
    cp /etc/aivyx/aivyx.appliance.toml "$_cfg"
    echo "aivyx-pa (entrypoint): seeded $_cfg from the appliance default" >&2
fi

# Chapter Gatehouse — the daemon refuses an off-host bind with no auth token
# (the exposure interlock), and the appliance binds beyond loopback by design.
# If the active config exposes the Studio but sets neither a token nor the
# explicit insecure escape hatch, generate a Studio token once, insert it right
# after the bind-host line (in the same section: a second [studio] table would
# be invalid TOML), and print it so the operator can log in. Idempotent:
# subsequent boots see the token in the file. Reads both spellings: `[studio]
# host` and the deprecated `[daemon] web_ui_host` (R21), and answers in the
# spelling the file uses.
# Prints `studio` or `daemon` (where the off-host bind is) when a token is
# needed, nothing otherwise.
_gatehouse_awk='
    /^[[:space:]]*\[/ {
        sec = $0; gsub(/^[[:space:]]*\[[[:space:]]*|[[:space:]]*\].*$/, "", sec); next
    }
    { line = $0; sub(/^[[:space:]]+/, "", line) }
    # The bind host as the daemon resolves it: `[studio] host` wins over the
    # deprecated `[daemon] web_ui_host` when both are set.
    sec == "studio" && line ~ /^host[[:space:]]*=/ {
        new_set = 1; new_off = (line !~ /=[[:space:]]*"(127\.|::1)/)
    }
    sec == "daemon" && line ~ /^web_ui_host[[:space:]]*=/ {
        old_set = 1; old_off = (line !~ /=[[:space:]]*"(127\.|::1)/)
    }
    (sec == "studio" && line ~ /^(auth_token|insecure_no_auth)[[:space:]]*=/) { auth = 1 }
    (sec == "daemon" && line ~ /^web_ui_(auth_token|insecure_no_auth)[[:space:]]*=/) { auth = 1 }
    END {
        if (new_set) { if (new_off) exposed = "studio" }
        else if (old_set && old_off) exposed = "daemon"
        if (exposed != "" && !auth) print exposed
    }'
_where="$(awk "$_gatehouse_awk" "$_cfg")"
if [ -n "$_where" ]; then
    _tok="$(tr -dc 'a-zA-Z0-9' < /dev/urandom | head -c 43)"
    if [ "$_where" = studio ]; then _host_key=host; _tok_key=auth_token
    else _host_key=web_ui_host; _tok_key=web_ui_auth_token; fi
    awk -v tok="$_tok" -v sec_want="$_where" -v hk="$_host_key" -v tk="$_tok_key" '
        /^[[:space:]]*\[/ { sec = $0; gsub(/^[[:space:]]*\[[[:space:]]*|[[:space:]]*\].*$/, "", sec) }
        { print; line = $0; sub(/^[[:space:]]+/, "", line) }
        !done && sec == sec_want && index(line, hk) == 1 && line ~ ("^" hk "[[:space:]]*=") {
            print tk " = \"" tok "\"  # generated at first boot (Gatehouse)"
            done = 1
        }' "$_cfg" > "$_cfg.new" && mv "$_cfg.new" "$_cfg"
    echo "aivyx-pa (entrypoint): generated a Studio auth token (Gatehouse):" >&2
    echo "aivyx-pa (entrypoint):   $_tok" >&2
    echo "aivyx-pa (entrypoint): browsers: any username, this token as the password." >&2
    echo "aivyx-pa (entrypoint): it is saved in $_cfg ([$_where] $_tok_key)." >&2
fi

exec aivyx-pa daemon run "$@"
