# Contacts (Chapter Contacts)

> **Status:** ✅ **shipped** (Chapter Contacts complete, CT.0–CT.5). This began
> as the design contract and is now implemented: two Trusted-tier scope bases
> (`contacts.read` / `contacts.write`, `aivyx-capability`), the `aivyx-contacts`
> third-party tool-process binary over the Google People API (cloned from the
> `aivyx-drive` OAuth-binary template, consuming `aivyx-google-oauth`), six
> tools (`contacts.search` / `list` / `get` + `create` / `update` / `delete`),
> and `aivyx-pa connect contacts` guided onboarding. The first **Broaden** chapter
> — the everyday-PA domain expansion seeded by finding **F4** of the
> [2026-06-16 backend audit](BACKEND_AUDIT_2026-06-16.md) ("the covered set
> still skews developer / knowledge-worker"). Contacts is the first slice:
> the agent learns *who the people in your life are*.
>
> **Open questions resolved as committed:** F-1 — `contacts.list` returns one
> page + `next_page_token` (not eager pagination). F-2 — `contacts.update`
> derives the `updatePersonFields` mask from the keys supplied, guarded by the
> required `etag`.
>
> Capabilities gate *what* a tool may do; budgets
> ([`COST_GOVERNANCE.md`](COST_GOVERNANCE.md)) gate *how much it spends*;
> Throttle ([`RATE_LIMITS.md`](RATE_LIMITS.md)) gates *how often it is called*.
> Contacts adds **a new domain the agent can reach**, on the same
> third-party-tool-process substrate every Chapter F integration already uses.

## 1. The gap — the assistant doesn't know your people

Aivyx PA can read your inbox (Gmail), your calendar, your files (Drive), your
notes (Notion / Obsidian), and run your automations (n8n). What it cannot do
is answer *"what's Dana's email?"* or *"add my new dentist to my contacts."*
For a personal assistant, the address book is foundational — almost every
everyday-PA task (drafting an email, scheduling a meeting, sending a reminder)
implicitly references a person, and today the agent has no place to resolve a
name to a real identity.

The backend audit named this **F4 — everyday-PA domain breadth**: contacts /
CRM, weather, maps, SMS, smart-home, media all absent. The operator picked
**Contacts** to open the Broaden track because it is the highest-leverage
missing primitive — it composes with every domain that already ships (resolve a
name → draft an email to it, create a calendar event with it, set a reminder
about it).

## 2. The key insight — mirror Drive, don't invent

This chapter introduces **zero new substrate.** Contacts is the fifth Google
integration, and the OAuth-binary template Chapter F established is reused
verbatim:

| Chapter F (Gmail/Calendar/Drive) | → Contacts (this chapter) |
|---|---|
| Separate binary per service (`aivyx-drive`) | new `aivyx-contacts` binary |
| Operator-provided Google OAuth client | same — reuses `aivyx-google-oauth` |
| Per-process token file (`~/.aivyx-pa/tool-processes/{service}/tokens.json`) | `.../contacts/tokens.json` |
| Multi-tool IPC harness (`aivyx-tool`) | same harness, no copy |
| `auth_cli/` (`init`/`status`/`revoke`) | same five-module CLI |
| `[[tool_process]]` operator config entry | same — no daemon spawn code |
| `drive.read` / `drive.write` scope split | `contacts.read` / `contacts.write` |

The data source is the **Google People API**
(`https://people.googleapis.com/v1`). `aivyx-drive` is the closest existing
template (read-mostly + a gated write surface), so CT.2 clones its crate layout
rather than starting from `aivyx-gmail`.

## 3. What's deliberately *not* here

- **Not a CRM / pipeline tool.** This is the address book — people, emails,
  phones, organizations. Deal stages, notes-per-contact, and third-party CRM
  (HubSpot/Salesforce) connectors are a later Broaden slice, not this chapter.
- **Not contact-group management.** People API `contactGroups` (labels) is out
  of scope for CT; the six tools operate on individual people.
- **Not "Other contacts" / directory.** Only the user's own editable
  connections (`people/me/connections`) and search over them. The read-only
  `otherContacts` surface is deferred.
- **No new trust tier or gate.** `contacts.write` is Trusted-tier-only at the
  ceiling exactly like every other Chapter F write base; `contacts.delete`
  rides the existing irreversible-op confirm-first policy.

## 4. The tool surface — six tools, two scopes

A 3-read / 3-write split mirroring `drive.*` (Q-equivalent: Drive shipped 7
tools over 2 bases; Contacts ships the People API's natural CRUD over 2 bases).

**`contacts.read`** (any tier that holds the grant):

1. **`contacts.search`** — `GET people:searchContacts?query=&readMask=` —
   fuzzy lookup over the user's connections. The name-resolution primitive.
2. **`contacts.list`** — `GET people/me/connections?personFields=` —
   paginated full list (page-size capped, `next_page_token` echoed).
3. **`contacts.get`** — `GET people/{resourceName}?personFields=` — full
   detail for one resolved person.

**`contacts.write`** (Trusted ceiling only by default):

4. **`contacts.create`** — `POST people:createContact` — add a person.
5. **`contacts.update`** — `PATCH people/{resourceName}:updateContact`
   `?updatePersonFields=` — requires the person's current `etag` (People API
   optimistic-concurrency); the tool surfaces `etag` on every read so the model
   can round-trip it.
6. **`contacts.delete`** — `DELETE people/{resourceName}:deleteContact` —
   irreversible; **confirm-first** per the standard policy (the tool requires an
   explicit `confirm: true` in its input, matching the Documents `delete`
   gate).

### Output trimming

People API returns deeply-nested `Person` objects (every field is an array of
`{metadata, value, ...}` records). Every tool trims to a flat, snake-cased
shape the LLM can use directly, matching the `web.search` / Gmail precedent:

```json
{
  "resource_name": "people/c123",
  "etag": "%EgU…",
  "display_name": "Dana Lee",
  "emails": ["dana@example.com"],
  "phones": ["+1 555 0100"],
  "organizations": ["Acme — Designer"]
}
```

## 5. Scopes & forensics

Two new bases in `KNOWN_BASES` (`aivyx-capability`), growing it **83 → 85**:

- `contacts.read` — `contacts.search`, `contacts.list`, `contacts.get`.
- `contacts.write` — `contacts.create`, `contacts.update`, `contacts.delete`
  (Trusted-tier only by default, matching the `email.* / calendar.* / drive.*`
  third-party-tool-process gating pattern Phase 123 established).

Both go in `CEILING_TRUSTED` only. SemiTrusted / Untrusted roles get zero
contacts access by default; an operator who wants narrow read access from a
remote channel grants `contacts.read` explicitly via the role's
`capability_scopes` (Phase 62 Q2(a)). The count assertion
(`known_bases_count_matches_phase_143_a3_addendum`) and the A3 addendum
enumeration move to **85** in the same change (the count-drift discipline from
Throttle TH.4).

OAuth: `DEFAULT_CONTACTS_SCOPES = ["https://www.googleapis.com/auth/contacts"]`
(the read-write People API scope; the narrower `contacts.readonly` is *not*
requested because the chapter ships the write surface). No tool executes
against a token whose `granted_scope` lacks it — the same startup check
`aivyx-drive` runs.

## 6. Config

No new config schema — Contacts is a `[[tool_process]]` like every Chapter F
binary. The operator adds one entry to `aivyx-pa.toml`:

```toml
[[tool_process]]
name = "contacts"
command = "aivyx-contacts"
# Optional per-tool scope overrides — operator CAN narrow,
# CANNOT widen (e.g. drop contacts.write to make the process
# read-only). The tools declare contacts.read / contacts.write
# themselves; the daemon checks them against the active role.
# [tool_process.scope_overrides]
# Optional per-tool expected-scope ceiling — validates (doesn't narrow)
# the process's self-declared scope; registration is refused if it
# isn't covered. See docs/TOOL_SDK.md §6.
# [tool_process.expected_scopes]
```

The fastest path is `aivyx-pa connect contacts`, which writes the
config, runs the consent flow, and offers to add the
`[[tool_process]]` entry for you. To do it by hand, run the
one-time OAuth handshake out-of-band:

```sh
aivyx-contacts auth init      # opens Google consent, writes tokens.json (0600)
aivyx-contacts auth status    # show granted scopes + expiry
aivyx-contacts auth revoke    # delete the local token file
```

OAuth client credentials live in
`~/.aivyx-pa/tool-processes/contacts/config.toml` (operator-supplied
`client_id` / `client_secret` / `redirect_uri`), identical to Drive.

## 7. Phase plan

| Phase | Deliverable |
|---|---|
| **CT.0** | This contract. |
| **CT.1** | `contacts.read` + `contacts.write` in `KNOWN_BASES` + `CEILING_TRUSTED`; count assertion 83→85; A3 addendum enumeration + count updated in the same change. Pure capability-layer change, unit-tested (both bases parse; Trusted holds them, SemiTrusted/Untrusted don't). |
| **CT.2** | `aivyx-contacts` crate scaffold cloned from `aivyx-drive`: `Cargo.toml` (workspace member, `dist = false`), `lib.rs` with `DEFAULT_CONTACTS_SCOPES`, `main.rs` multi-tool harness entry, `auth_cli/` (cli/config_file/init/revoke/status) over `aivyx-google-oauth`, `contacts_client.rs` People API client shell + `with_api_base_url` test seam. Compiles + auth-CLI smoke test. |
| **CT.3** | Read tools — `contacts.search`, `contacts.list`, `contacts.get` — each a `Tool` impl with the §4 trimmed output, `required_scope = contacts.read`. Unit-tested against an in-process mock People API (mirrors Drive's tool tests). |
| **CT.4** | Write tools — `contacts.create`, `contacts.update` (etag round-trip), `contacts.delete` (`confirm: true` gate) — `required_scope = contacts.write`. Mock-API unit tests incl. the delete confirm-first refusal path. |
| **CT.5** | Finalize — full workspace build + test, the `[[tool_process]]` example above verified, flip this doc's Status to shipped, update the backend-audit F4 note (Contacts landed), write the chapter memory. |

## 8. Open questions (resolved at CT.N)

- **F-1 (CT.3):** does `contacts.list` paginate eagerly (walk all pages up to a
  cap) or return one page + `next_page_token`? *Lean: one page + token, like
  Drive's `list_folder` — cheaper, and the model can ask for more.*
- **F-2 (CT.4):** does `contacts.update` accept a partial person (only the
  fields to change) and compute `updatePersonFields` from the keys present, or
  require the caller to name the mask explicitly? *Lean: derive the mask from
  the supplied keys — friendlier to the LLM, and the etag guards concurrency.*
