# The desktop app

Aivyx PA has a **native desktop app** — an alternative to opening the Studio in a
browser. It puts your assistant in your system tray (menu bar), so it's always a
click away, and it can notify you when something needs your attention even when
no window is open.

It's the same Studio you already know, just in its own window with some native
extras. Everything in this guide works exactly the same way there.

## What it adds

- **A tray / menu-bar icon** — your assistant lives there, always running. Click
  it to open the Studio.
- **Hide-to-tray** — closing the window tucks the app into the tray instead of
  shutting it down, so your assistant keeps working in the background.
- **Approval notifications** — when a mission pauses for your sign-off, you get a
  normal OS notification. Click it to jump straight to the Studio. You no longer
  have to keep a tab open to catch an approval.
- **A global hotkey** — press **Ctrl + Shift + A** from anywhere to summon (or
  hide) the window.
- **Start at login** — optionally have Aivyx PA launch automatically when you sign
  in, so it's ready whenever you are.

## The tray menu

Right-click (or click) the tray icon for:

- **Open Studio** — show and focus the window.
- **Restart daemon** — restart the background service if something seems stuck.
- **Start at login** — toggle launch-on-login on or off.
- **Quit Aivyx PA** — fully exit (this stops the background service too).

## Getting it

The desktop app is installed separately from the command-line version. See the
[installation guide](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md#desktop-app)
for downloads and the build-from-source steps.

> **On Linux** the desktop app needs a few system packages (a webview and the
> tray libraries) — the install guide lists them for your distribution. macOS
> works out of the box. A native Windows version isn't available yet.

## Daemon and the desktop app

Like every Aivyx PA interface, the desktop app is a window onto the background
service (the *daemon*). It starts the daemon for you if it isn't already running
and attaches to it if it is — so your conversation, memory, and settings are the
same whether you use the desktop app, the browser, or the terminal.
