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
use std::process::{Child, Command, Stdio};
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

mod first_run;
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
    /// First-run E — the setup page's **Try again** button.
    Retry,
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

/// The `aivyx-pa` binary to drive the daemon: `AIVYX_PA_BIN` if set, else the
/// one installed beside this app, else `aivyx-pa` on `PATH`.
fn aivyx_bin() -> String {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("aivyx-pa"))) // instance-paths: ok — the aivyx-pa binary beside this one, not a data path
        .filter(|p| p.is_file());
    first_run::pick_aivyx_bin(std::env::var("AIVYX_PA_BIN").ok(), sibling)
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

/// Where the daemon this app starts writes its output (`None` if there's no
/// home to put it in — the output is then dropped).
fn daemon_log() -> Option<std::path::PathBuf> {
    first_run::daemon_log_path(
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

/// Ensure a daemon is serving the Studio: attach if one is up (no child),
/// else spawn `aivyx-pa daemon run --web-ui` (env inherited, output to
/// [`daemon_log`]) and return the child so we can stop it on quit. When the
/// Studio still isn't there, the [`first_run::SetupProblem`] says why, for
/// the page shown instead (First-run E).
fn ensure_daemon() -> (Option<Child>, Option<first_run::SetupProblem>) {
    if daemon_reachable() {
        return (None, None);
    }
    if !studio_is_local() {
        eprintln!(
            "aivyx-desktop: remote Studio {} is unreachable — check the appliance.",
            studio_url()
        );
        return (None, None);
    }
    let log_path = daemon_log();
    let log_file = log_path.as_ref().and_then(|path| {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::File::create(path).ok()
    });
    let (out, err) = match log_file.and_then(|f| Some((f.try_clone().ok()?, f))) {
        Some((out, err)) => (Stdio::from(out), Stdio::from(err)),
        None => (Stdio::null(), Stdio::null()),
    };
    let bin = aivyx_bin();
    let mut child = match Command::new(&bin)
        .args(["daemon", "run", "--web-ui"])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("aivyx-desktop: could not spawn the daemon: {e}");
            let problem = first_run::SetupProblem::NotInstalled {
                bin,
                error: e.to_string(),
            };
            return (None, Some(problem));
        }
    };
    wait_until_reachable();
    if daemon_reachable() {
        return (Some(child), None);
    }
    let log_path = log_path.unwrap_or_default();
    match child.try_wait() {
        Ok(Some(_)) => {
            let output = std::fs::read_to_string(&log_path).unwrap_or_default();
            let problem = first_run::SetupProblem::Exited {
                log_tail: first_run::tail_lines(&output, 8),
                log_path,
            };
            (None, Some(problem))
        }
        _ => (Some(child), Some(first_run::SetupProblem::NoAnswer { log_path })),
    }
}

/// Start the approval-gate watcher: its own thread + tokio runtime, polling
/// the daemon for missions awaiting approval and firing OS notifications.
/// Only once the daemon is up — it gives up for the run on a missing token.
fn start_gate_watcher(proxy: tao::event_loop::EventLoopProxy<UserEvent>, token: Option<String>) {
    std::thread::spawn(move || {
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(gate_watch::run(proxy, token)),
            Err(e) => eprintln!("aivyx-desktop: gate watcher runtime failed: {e}"),
        }
    });
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
    let (mut daemon_child, mut problem) = ensure_daemon();
    // After `ensure_daemon`, so a daemon that just started has created its
    // automatic token file.
    let mut token = if problem.is_none() { studio_token() } else { None };

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

    let mut watcher_started = false;
    if problem.is_none() {
        start_gate_watcher(proxy.clone(), token.clone());
        watcher_started = true;
    }

    let window = WindowBuilder::new()
        .with_title("Aivyx PA Studio")
        .with_inner_size(LogicalSize::new(1280.0, 820.0))
        .with_min_inner_size(LogicalSize::new(720.0, 480.0))
        .build(&event_loop)
        .expect("failed to create the window");

    let retry_proxy = proxy.clone();
    let on_ipc = move |message: &str| {
        if message == "retry" {
            let _ = retry_proxy.send_event(UserEvent::Retry);
        }
    };
    let first_page = match &problem {
        None => Page::Url(studio_entry_url(token.as_deref())),
        Some(p) => Page::Html(first_run::setup_page_html(p)),
    };
    let webview = build_webview(&window, first_page, on_ipc)?;

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
                    (daemon_child, problem) = ensure_daemon();
                    show_studio_or_setup(&webview, &mut problem, &mut token);
                    if problem.is_none() && !watcher_started {
                        start_gate_watcher(proxy.clone(), token.clone());
                        watcher_started = true;
                    }
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
            // First-run E — the setup page's Try again: reuse a daemon that's
            // still starting, else start one, then show the Studio or the
            // (updated) setup page.
            Event::UserEvent(UserEvent::Retry) => {
                if !daemon_reachable() {
                    let exited = daemon_child
                        .as_mut()
                        .is_none_or(|c| matches!(c.try_wait(), Ok(Some(_))));
                    if exited {
                        (daemon_child, problem) = ensure_daemon();
                    } else {
                        wait_until_reachable();
                    }
                }
                if daemon_reachable() {
                    problem = None;
                } else if problem.is_none() {
                    problem = Some(first_run::SetupProblem::NoAnswer {
                        log_path: daemon_log().unwrap_or_default(),
                    });
                }
                show_studio_or_setup(&webview, &mut problem, &mut token);
                if problem.is_none() && !watcher_started {
                    start_gate_watcher(proxy.clone(), token.clone());
                    watcher_started = true;
                }
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

/// What the webview opens on: the Studio, or the local setup page.
enum Page {
    Url(String),
    Html(String),
}

/// Point the webview at the Studio (fetching a fresh sign-in token, since a
/// daemon that just started has just created it), or at the setup page when
/// there's a `problem`.
fn show_studio_or_setup(
    webview: &wry::WebView,
    problem: &mut Option<first_run::SetupProblem>,
    token: &mut Option<String>,
) {
    match problem {
        None => {
            *token = studio_token();
            let _ = webview.load_url(&studio_entry_url(token.as_deref()));
        }
        Some(p) => {
            let _ = webview.load_html(&first_run::setup_page_html(p));
        }
    }
}

/// The webview's builder, with the first page and the IPC handler the setup
/// page's **Try again** posts to.
fn webview_builder<'a>(
    page: Page,
    on_ipc: impl Fn(&str) + 'static,
) -> WebViewBuilder<'a> {
    let builder = WebViewBuilder::new()
        .with_ipc_handler(move |request: wry::http::Request<String>| on_ipc(request.body()));
    match page {
        Page::Url(url) => builder.with_url(url),
        Page::Html(html) => builder.with_html(html),
    }
}

/// Build the webview into the window. On Linux the webview attaches to the
/// window's GTK vbox (the documented wry+tao pattern); elsewhere it builds from
/// the raw window handle.
#[cfg(not(target_os = "linux"))]
fn build_webview(
    window: &Window,
    page: Page,
    on_ipc: impl Fn(&str) + 'static,
) -> wry::Result<wry::WebView> {
    webview_builder(page, on_ipc).build(window)
}

#[cfg(target_os = "linux")]
fn build_webview(
    window: &Window,
    page: Page,
    on_ipc: impl Fn(&str) + 'static,
) -> wry::Result<wry::WebView> {
    use tao::platform::unix::WindowExtUnix;
    use wry::WebViewBuilderExtUnix;
    let vbox = window
        .default_vbox()
        .expect("tao window should expose a default GTK vbox on Linux");
    webview_builder(page, on_ipc).build_gtk(vbox)
}
