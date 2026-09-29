//! `aivyx-desktop` — the native desktop shell for Aivyx.
//!
//! A thin native window that hosts the **Studio** (the Dioxus web UI the daemon
//! already serves at `http://127.0.0.1:7843`) inside a system webview, plus the
//! daemon lifecycle and a system tray. This is *native chrome over the existing
//! web UI*, not a second UI — the webview runs the exact same WASM Studio a
//! browser would.
//!
//! Phases: (1) window + webview + ensure-daemon, (2 — this file) **system tray**
//! (Open Studio / Restart daemon / Quit) with hide-to-tray on close. Native gate
//! notifications, a global hotkey, and the installer land in later phases.
//!
//! Linux note: the webview is WebKitGTK (`webkit2gtk-4.1`) and the tray uses an
//! appindicator (`libayatana-appindicator`) — the shell's native dependencies.

use std::net::TcpStream;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use auto_launch::AutoLaunchBuilder;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIconBuilder, TrayIconEvent};
use wry::WebViewBuilder;

mod gate_watch;

/// Where the daemon serves the Studio (HTTP + the `/ws` WebSocket bridge).
const STUDIO_URL: &str = "http://127.0.0.1:7843/";
const STUDIO_ADDR: &str = "127.0.0.1:7843";

/// The Studio URL the shell wraps: `AIVYX_PA_STUDIO_URL` when set (a remote
/// Aivyx — the Harbor/server-appliance topology, e.g.
/// `http://10.80.80.148:7843/`), else the local default. Vitrine §12 —
/// the shell was hardcoded to localhost, which made it unusable against
/// a rig-hosted Studio.
fn studio_url() -> String {
    std::env::var("AIVYX_PA_STUDIO_URL").unwrap_or_else(|_| STUDIO_URL.to_string())
}

/// Host:port derived from [`studio_url`], for the reachability probe.
fn studio_addr() -> String {
    let url = studio_url();
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or(STUDIO_ADDR)
        .to_string()
}

/// Only a localhost Studio is ours to manage: spawning or stopping a
/// daemon makes no sense against a remote appliance.
fn studio_is_local() -> bool {
    let a = studio_addr();
    a.starts_with("127.0.0.1") || a.starts_with("localhost")
}

/// Events we route into the single tao event loop — from the tray's global
/// channels and from the background gate watcher — so everything is handled in
/// one place.
pub(crate) enum UserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
    /// Raise + focus the window (a tray click, or a notification's "Open").
    ShowWindow,
    /// The global hotkey fired — toggle the window's visibility.
    ToggleWindow,
}

/// Build the launch-on-login controller for this executable. `None` if the
/// platform autostart entry can't be constructed.
fn auto_launch() -> Option<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    AutoLaunchBuilder::new()
        .set_app_name("Aivyx PA")
        .set_app_path(&exe.to_string_lossy())
        .build()
        .ok()
}

/// The `aivyx-pa` binary to drive the daemon: `AIVYX_PA_BIN` if set, else `aivyx-pa` on
/// `PATH`.
fn aivyx_bin() -> String {
    std::env::var("AIVYX_PA_BIN").unwrap_or_else(|_| "aivyx-pa".to_string())
}

/// The Studio sign-in token: `AIVYX_PA_STUDIO_TOKEN` when set, else — for a
/// localhost Studio — whatever `aivyx-pa studio --token` reports (it reads the
/// daemon's `0600` token file as this user, resolving the store path exactly
/// as the daemon does). `None`: no token needed, or none could be found.
fn studio_token() -> Option<String> {
    if let Ok(t) = std::env::var("AIVYX_PA_STUDIO_TOKEN") {
        if !t.trim().is_empty() {
            return Some(t.trim().to_string());
        }
    }
    if !studio_is_local() {
        return None;
    }
    let out = Command::new(aivyx_bin()).args(["studio", "--token"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// Where the webview opens: the one-time sign-in link when there's a token
/// (it plants the auth cookie and redirects to `/`), else the Studio itself.
fn studio_entry_url(token: Option<&str>) -> String {
    match token {
        Some(t) => format!("{}/?token={t}", studio_url().trim_end_matches('/')),
        None => studio_url(),
    }
}

/// Is the daemon's Studio reachable right now?
fn daemon_reachable() -> bool {
    let Ok(addr) = studio_addr().parse() else {
        return false;
    };
    TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_ok()
}

/// Block (up to ~12s) until the daemon is serving, so the first webview load
/// lands on a live Studio.
fn wait_until_reachable() {
    let start = Instant::now();
    while !daemon_reachable() && start.elapsed() < Duration::from_secs(12) {
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Ensure a daemon is serving the Studio: attach if one is up (returns `None`),
/// else spawn `aivyx-pa daemon run --web-ui` (env inherited) and return the child
/// so we can stop it on quit. A spawn failure is non-fatal.
fn ensure_daemon() -> Option<Child> {
    if daemon_reachable() {
        return None;
    }
    if !studio_is_local() {
        eprintln!(
            "aivyx-desktop: remote Studio {} is unreachable — check the appliance.",
            studio_url()
        );
        return None;
    }
    match Command::new(aivyx_bin())
        .args(["daemon", "run", "--web-ui"])
        .spawn()
    {
        Ok(child) => {
            wait_until_reachable();
            Some(child)
        }
        Err(e) => {
            eprintln!("aivyx-desktop: could not spawn the daemon: {e}");
            eprintln!("aivyx-desktop: start it yourself, then reopen — the window will connect.");
            None
        }
    }
}

/// Stop a daemon we own (graceful `aivyx-pa daemon stop`, then reap the child).
fn stop_owned_daemon(child: &mut Option<Child>) {
    if child.is_none() {
        return;
    }
    let _ = Command::new(aivyx_bin()).args(["daemon", "stop"]).status();
    if let Some(mut c) = child.take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

/// Decode the embedded brand PNG into a tray icon.
fn tray_icon_image() -> tray_icon::Icon {
    let bytes = include_bytes!("../assets/tray-icon.png");
    let decoder = png::Decoder::new(&bytes[..]);
    let mut reader = decoder.read_info().expect("valid tray PNG");
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).expect("decode tray PNG");
    let rgba = buf[..info.buffer_size()].to_vec();
    tray_icon::Icon::from_rgba(rgba, info.width, info.height).expect("RGBA -> tray Icon")
}

fn main() -> wry::Result<()> {
    let mut daemon_child = ensure_daemon();
    // After `ensure_daemon`, so a daemon that just started has created its
    // automatic token file.
    let token = studio_token();

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();

    // Route the tray's global menu/icon events into this event loop.
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e));
    }));
    let tray_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e| {
        let _ = tray_proxy.send_event(UserEvent::Tray(e));
    }));

    // Global hotkey (Ctrl+Shift+A) to summon the window. Best-effort: a
    // Wayland-only session can't grab globally, so a failure is logged and the
    // rest of the app runs normally. The manager must outlive the loop — kept
    // in `_hotkey_manager` (the diverging `run()` below never drops locals).
    let hotkey_proxy = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            let _ = hotkey_proxy.send_event(UserEvent::ToggleWindow);
        }
    }));
    let _hotkey_manager = match GlobalHotKeyManager::new() {
        Ok(mgr) => {
            let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyA);
            if let Err(e) = mgr.register(hotkey) {
                eprintln!("aivyx-desktop: could not register the global hotkey: {e}");
            }
            Some(mgr)
        }
        Err(e) => {
            eprintln!("aivyx-desktop: global hotkeys unavailable (Wayland-only?): {e}");
            None
        }
    };

    // Launch-on-login controller (None if the platform entry can't be built).
    let autostart = auto_launch();

    // Background approval-gate watcher: its own thread + tokio runtime, polling
    // the daemon for missions awaiting approval and firing OS notifications.
    {
        let watcher_proxy = proxy.clone();
        let watcher_token = token.clone();
        std::thread::spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(gate_watch::run(watcher_proxy, watcher_token)),
                Err(e) => eprintln!("aivyx-desktop: gate watcher runtime failed: {e}"),
            }
        });
    }

    let window = WindowBuilder::new()
        .with_title("Aivyx PA Studio")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0))
        .build(&event_loop)
        .expect("failed to create the window");

    let webview = build_webview(&window, &studio_entry_url(token.as_deref()))?;

    // Tray menu: Open Studio · Restart daemon · Quit. Built after the event loop
    // (GTK is initialized by then on Linux).
    let menu = Menu::new();
    let open_item = MenuItem::new("Open Studio", true, None);
    let restart_item = MenuItem::new("Restart daemon", true, None);
    // Reflects the current autostart state; toggling enables/disables it.
    let autostart_checked = autostart
        .as_ref()
        .and_then(|a| a.is_enabled().ok())
        .unwrap_or(false);
    let autostart_item =
        CheckMenuItem::new("Start at login", autostart.is_some(), autostart_checked, None);
    let quit_item = MenuItem::new("Quit Aivyx PA", true, None);
    menu.append_items(&[
        &open_item,
        &PredefinedMenuItem::separator(),
        &restart_item,
        &autostart_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])
    .expect("build tray menu");
    let _tray = TrayIconBuilder::new()
        .with_tooltip("Aivyx PA")
        .with_icon(tray_icon_image())
        .with_menu(Box::new(menu))
        .build()
        .expect("build tray icon");

    let open_id = open_item.id().clone();
    let restart_id = restart_item.id().clone();
    let autostart_id = autostart_item.id().clone();
    let quit_id = quit_item.id().clone();

    // Track visibility ourselves so the hotkey can toggle reliably across
    // platforms (avoids the platform-specific `Window::is_visible`).
    let mut visible = true;
    let show = |window: &Window, visible: &mut bool| {
        window.set_visible(true);
        window.set_focus();
        *visible = true;
    };

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // Closing the window hides to the tray and keeps the daemon running
            // (the always-on-assistant model) — "Quit Aivyx PA" is the real exit.
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                window.set_visible(false);
                visible = false;
            }
            Event::UserEvent(UserEvent::Menu(e)) => {
                if e.id == open_id {
                    show(&window, &mut visible);
                } else if e.id == restart_id {
                    stop_owned_daemon(&mut daemon_child);
                    daemon_child = ensure_daemon();
                    let _ = webview.load_url(&studio_entry_url(token.as_deref()));
                } else if e.id == autostart_id {
                    // The CheckMenuItem flipped its own checkmark; sync the
                    // platform autostart entry to the new state.
                    if let Some(a) = autostart.as_ref() {
                        let want_on = autostart_item.is_checked();
                        let res = if want_on { a.enable() } else { a.disable() };
                        if let Err(e) = res {
                            eprintln!("aivyx-desktop: could not update launch-on-login: {e}");
                            autostart_item.set_checked(!want_on); // revert the UI
                        }
                    }
                } else if e.id == quit_id {
                    stop_owned_daemon(&mut daemon_child);
                    *control_flow = ControlFlow::Exit;
                }
            }
            // A tray left-click or a notification's "Open" raises the window.
            Event::UserEvent(UserEvent::Tray(TrayIconEvent::Click { .. }))
            | Event::UserEvent(UserEvent::ShowWindow) => {
                show(&window, &mut visible);
            }
            // The global hotkey toggles the window.
            Event::UserEvent(UserEvent::ToggleWindow) => {
                if visible {
                    window.set_visible(false);
                    visible = false;
                } else {
                    show(&window, &mut visible);
                }
            }
            _ => {}
        }
    });
}

/// Build the webview into the window. On Linux the webview attaches to the
/// window's GTK vbox (the documented wry+tao pattern); elsewhere it builds from
/// the raw window handle.
#[cfg(not(target_os = "linux"))]
fn build_webview(window: &Window, url: &str) -> wry::Result<wry::WebView> {
    WebViewBuilder::new().with_url(url).build(window)
}

#[cfg(target_os = "linux")]
fn build_webview(window: &Window, url: &str) -> wry::Result<wry::WebView> {
    use tao::platform::unix::WindowExtUnix;
    use wry::WebViewBuilderExtUnix;
    let vbox = window
        .default_vbox()
        .expect("tao window should expose a default GTK vbox on Linux");
    WebViewBuilder::new().with_url(url).build_gtk(vbox)
}
