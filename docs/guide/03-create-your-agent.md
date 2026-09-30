# Create your agent

This is the guided setup that gives your assistant its identity. You can run it
in the terminal (`aivyx-pa init`) or in the Studio's **Create** screen — both do the
same thing and write to the same place.

It has three steps: **Profile → Persona seed → Access**.

## Step 1 — Profile (who your assistant is)

The Profile is the identity *you declare*. It has six parts:

1. **Name** — what you'll call your assistant.
2. **About you** — a little about you, so it can tailor how it helps.
3. **How it talks** — tone and style (warm and chatty, terse and technical, …).
4. **What it's for** — the kinds of tasks you want help with.
5. **What it tends to do** — default behaviors and habits you want.
6. **Lines it must never cross** — hard boundaries it will always respect.

You can fill these in two ways:

- **Let the model draft it.** Answer a few questions about the relationship you
  want, and your chosen model writes a full first draft. You then review and
  edit every line — you're always the final author.
- **Write it yourself.** The same six fields with helpful prompts, fully offline.
  No model required.

It ends with a friendly "meet your assistant" preview you can confirm, edit, or
restart.

> The Profile is who your assistant *starts as*. Its deeper character — the
> Persona — is *earned* over time as it learns from working with you. You shape
> the starting point here; you don't have to get it perfect.

## Step 2 — Persona seed (its starting character)

Optionally give your assistant a short starting personality and a few starter
skills — a "seed" for the character it will grow into. You can describe it in
your own words, or let the model draft one. Leave it blank and your assistant
simply starts as a clean slate and learns from there.

This seed is planted once, at first launch, and is fully auditable and
reversible later from the **Agents** screen.

## Step 3 — Access (how far it can reach)

Finally, choose how much of your machine the assistant can touch. There are
three levels:

- **Sandbox** — it can only read and write inside one dedicated folder. Safest;
  good for trying things out.
- **Home** — it can reach your home directory (documents, projects, …).
- **Full** — broad access to your filesystem.

Whatever you pick, **irreversible actions always stop for your confirmation** —
deleting or overwriting a file, sending an email, spending money. Access level
sets the *reach*; the confirmation step is your safety net on top.

You can change the level any time later (the **Settings** screen, or
`aivyx-pa access`).

## After setup

That's it — your assistant is ready. Everything you chose here is editable later:

- Edit the Profile from the **Agents** screen.
- Review and approve the character changes your assistant proposes as it learns —
  also on **Agents**.
- Change access and budgets on **Settings**.
