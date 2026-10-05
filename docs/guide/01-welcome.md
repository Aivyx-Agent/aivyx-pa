# Welcome to Aivyx PA

Aivyx PA is a personal AI assistant that runs **on your own hardware**. It learns
how you work, keeps its memory in an encrypted file on your machine, and records
everything it does in a tamper-evident log you can inspect at any time. There is
no Aivyx PA cloud — your assistant talks directly to whichever AI model you choose
(a local model with no API key, or a provider like Anthropic or OpenAI).

This guide is the end-user manual. It walks you from first launch through every
part of the Studio — the web interface you're reading this in.

## What makes Aivyx PA different

- **It's yours.** Your data, your model choice, your machine. Nothing is sent to
  a hosted service you don't control.
- **It learns.** Beyond the personality you give it at setup, your assistant
  refines how it helps you over time — and you approve every change.
- **It's accountable.** Every action it takes is written to a cryptographically
  chained audit log. You can always see exactly what happened.
- **It asks before doing anything risky.** Sending an email, deleting a file, or
  spending money always stops for your confirmation.

## How to read this guide

If you're brand new, start with **Getting started** and then **Create your
agent** — those two get you to a working assistant. After that, browse the pages
for whichever features you want to use: the Studio screens, the terminal,
autonomy and routines, security, and backups each have a page.

For exhaustive detail — every command, config setting and tool — see the
[reference manual](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/manual/README.md).

## The big picture

Aivyx PA runs as a small background service (the *daemon*) on your computer. Every
way you talk to your assistant — this web Studio, the [desktop app](11-desktop-app.md),
the terminal, voice, or [chat apps](13-chat-apps-and-accounts.md) — is just a
different window onto that same daemon. Close a window and the assistant keeps
running; your conversation, memory, and settings persist.

You can also run [several separate assistants](16-named-instances.md) side by
side — say one for research and one for the household — each with its own
memory, personality and Studio.
