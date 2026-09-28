# Two Keys at the Door — the exposure interlock (Chapter Gatehouse)

> **Status: COMPLETE (GH.0–GH.1, 2026-07-04).** v1.0-runway decision 2,
> locked 2026-07-04. Chapter Postern already built the Studio's auth
> *mechanism* (a `[daemon] web_ui_auth_token` checked constant-time at
> the `/ws` control plane and the page load: Bearer / HTTP Basic /
> cookie) but chose warn-don't-refuse for the unauthenticated off-host
> bind. Gatehouse turns that warning into a **two-key launch**: an
> unauthenticated agent with filesystem and shell reach can never be
> exposed to a network *by accident* — the ~175k-exposed-Ollama-hosts
> lesson, applied before v1.0's appliance story invites it.

## What shipped (GH.1)

- **The interlock, enforced at config load** (not at bind time — a
  bind-task failure would log-and-limp; a config error stops the daemon
  with the resolution block): `web_ui_host` beyond loopback with no
  `web_ui_auth_token` is a `ConfigError` naming both remedies, unless
  the operator explicitly signs the risk with the new
  `[daemon] web_ui_insecure_no_auth = true` (the
  behind-my-own-authenticating-reverse-proxy escape hatch — TLS and
  fancier auth legitimately remain proxy jobs). Loopback installs are
  byte-identical.
- **The Harbor appliance generates its token at first boot**: the
  entrypoint inserts a 43-char alphanumeric token (256-bit, URL-safe by
  construction) into the seeded config's `[daemon]` section and prints
  it once to the container log — the appliance stays
  works-out-of-the-box under the interlock instead of refusing to
  start. Operator-set tokens and the escape hatch are respected
  (idempotent, guarded on both keys' absence).
- Four-quadrant config tests (refused / hatch / token / loopback) + an
  offline test of the entrypoint insertion.

## Known gap — the interlock says nothing about the host's own firewall

`VITRINE.md` §0 (2026-07-05 rebuild baptism): UFW on the rig allowed
port 22 and silently dropped 7843 — every Aivyx PA-side check was green
(bind succeeded, token valid, cookie planted, `/ws` upgraded) while the
operator's browser simply never populated, with zero feedback pointing
at the real cause. The interlock above only reasons about Aivyx PA's own
config; it has no visibility into (and can't reach into) the host's
packet-filtering rules. **2026-08-27:** the non-loopback startup
warning (`aivyx-channel/src/web_ui.rs`) now explicitly names this —
"if a client can't connect even though this process is bound and
healthy, check the host's own firewall (ufw/firewalld/iptables)" — see
`docs/INSTALL.md`'s off-host exposure section for the same pointer.
Not a code fix (there's nothing in-process to fix), a diagnosability
one: the next operator hitting this gets pointed at the real cause
from the daemon's own log instead of needing a packet capture.

## Deliberately not here

- No accounts, sessions, or OIDC — Nexus/Passport-era.
- ~~No token generation in `aivyx-pa daemon install` for native
  installs.~~ **Reversed 2026-09-29 (first-run coherence A2):** the
  Studio is now on by default, and a loopback bind is reachable by every
  local account on the machine — "loopback-default" was never a boundary
  between users, only against the network. So every native install now
  gets a token without asking: when neither `web_ui_auth_token` nor
  `web_ui_insecure_no_auth` is set, the daemon itself (not `daemon
  install`) generates one into `studio-token` next to the store (256-bit,
  43 alphanumeric, `0600`, atomic, reused; Ward-denied to the agent) and
  prints a one-time sign-in link, `/?token=…`, which redirects and plants
  the cookie. The interlock above is unchanged: it still governs a
  configured host beyond loopback. See `docs/THREAT_MODEL.md` §4.11.
- The Studio needs no token prompt of its own — the sign-in link (or
  Postern's HTTP Basic page-load prompt) plants the cookie the `/ws`
  upgrade carries.
