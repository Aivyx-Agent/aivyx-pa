# Packs

A **pack** sets up a ready-made assistant for one kind of work — say,
running a small business: its profile, a team, skills and routines, already
written. Packs are signed by their publisher, and Aivyx PA checks the
signature before it installs anything.

## Where to get them

The **Aivyx Hub** is the official place for Aivyx packs. Each pack there
says what it needs (the Aivyx PA version, and any accounts or tools) and
gives the command to install it. You can also install a pack file someone
gives you, as long as you trust its publisher.

## Look before you install

```sh
aivyx-pa pack inspect business-manager-0.1.0.aivyxpack
```

`inspect` checks the signature and shows what the pack contains: its
routines, the autonomy it asks for, the accounts it expects, and whether it
passes its own checks. Nothing is installed.

## Install

```sh
aivyx-pa pack install business-manager-0.1.0.aivyxpack
```

Installing a pack creates a **new assistant** next to your existing one (a
[named instance](16-named-instances.md), called after the pack); your
current assistant isn't changed. Name it yourself with
`aivyx-pa --instance shop pack install …`.

You then go through the usual setup with the pack's answers pre-filled, and
it ends by listing what to connect — for example
`aivyx-pa --instance business-manager connect gmail`. Use the new assistant
by adding `--instance <name>` to any command, or open its Studio with
`aivyx-pa --instance <name> studio`.

## What a pack can't change

A pack can't set autonomy above **supervised**, and it can't decide how far
the assistant reaches on your machine, whether its Studio is exposed, which
programs it runs or where its requests go. Those stay your choices; change
them afterwards like any other setting.

## Tools a pack uses

Some packs use Aivyx's tool programs — Gmail, Calendar, Drive, Contacts,
the Toolkit. The one-line installer and the desktop app ship `aivyx-pa`
only; the Docker image includes every tool. To add one to an install, see
[the tool programs](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md#the-tool-programs-gmail-calendar-drive-contacts-notion-obsidian-n8n-toolkit)
in the install guide. `aivyx-pa connect gmail` tells you if one is missing.

## Trusting a publisher

Aivyx PA installs only packs signed by a publisher you trust. Until
Aivyx PA 1.0 builds the Aivyx keys in, add them yourself: copy the keys from
the Hub's `publisher-key.txt` into your default assistant's
`aivyx-pa.toml` (`~/.config/aivyx-pa/aivyx-pa.toml`; a pack's new assistant
uses this list too),

```toml
[pack]
trusted_publishers = ["<base64 key>"]
```

and `aivyx-pa pack inspect` will accept packs signed with them.
`--allow-untrusted` lets `inspect` show an untrusted pack anyway; `install`
never takes one.
