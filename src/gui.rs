//! The app window: the system webview (WebView2 / WebKit) showing the local server's page.
//!
//! The window opens at once on a splash page; the vault is indexed and its theme read in the background,
//! then the webview moves to the local server's page.
use fastobsidian::page::{enabled_plugins, obsidian_css};
use fastobsidian::server::{media_file, FolderPicker, Options, Server, ServerHandle};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;
use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::{Icon, Window, WindowBuilder};
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

/// Fixed so the page's origin, and with it the remembered light/dark choice, stays the same between launches.
const PREFERRED_PORT: u16 = 47615;

/// Shown while the vault loads.
const SPLASH: &str = include_str!("../splash.html");
const SPLASH_SIZE: LogicalSize<f64> = LogicalSize::new(380.0, 150.0);
const APP_SIZE: LogicalSize<f64> = LogicalSize::new(1400.0, 900.0);

/// Place the window in the middle of its monitor, for a window of logical `size`.
fn center(window: &Window, size: LogicalSize<f64>) {
    let Some(m) = window.current_monitor() else { return };
    let (area, pos, scale) = (m.size(), m.position(), m.scale_factor());
    let s: PhysicalSize<f64> = size.to_physical(scale);
    let x = pos.x as f64 + ((area.width as f64 - s.width) / 2.0).max(0.0);
    let y = pos.y as f64 + ((area.height as f64 - s.height) / 2.0).max(0.0);
    window.set_outer_position(PhysicalPosition::new(x, y));
}

/// The window and taskbar icon. On Windows it is the icon embedded in the .exe (by build.rs), so each size is
/// drawn sharp; elsewhere, or if that is missing, the same crystal drawn at 64×64.
fn app_icon() -> Option<Icon> {
    #[cfg(all(windows, target_env = "msvc"))]
    {
        use tao::platform::windows::IconExtWindows;
        if let Ok(icon) = Icon::from_resource(1, None) {
            return Some(icon);
        }
    }
    Icon::from_rgba(crate::icon::crystal_rgba(64), 64, 64).ok()
}

enum UserEvent {
    PickFolder(mpsc::Sender<Option<PathBuf>>),
    CloseAnswer {
        dirty: bool,
    },
    Title(String),
    /// the splash page has loaded and can run scripts
    SplashReady,
    /// a script for the splash page
    Splash(String),
    /// the server is up: show the app at this URL
    Ready(String),
    /// loading is taking a while: show the splash (ignored once the app is up)
    ShowSplash,
    /// the app page has drawn its first screen
    AppReady,
    /// show the window whatever it holds (fallback if no page-load event arrives)
    Show,
}

/// Loads faster than this never show the splash, so a quick start does not flash it.
const SPLASH_DELAY: Duration = Duration::from_millis(600);
/// Show the app window even if its page has not reported loading by then.
const SHOW_FALLBACK: Duration = Duration::from_secs(2);

/// With FO_TIMING set, print how long after start-up each phase finished (to stderr).
pub fn mark(phase: &str) {
    static T0: OnceLock<std::time::Instant> = OnceLock::new();
    let t0 = T0.get_or_init(std::time::Instant::now);
    if std::env::var_os("FO_TIMING").is_some() {
        eprintln!("timing {:>7.1} ms  {phase}", t0.elapsed().as_secs_f64() * 1000.0);
    }
}

/// Turn the (hidden or splash) window into the full app window, once.
fn show_app(window: &Window, done: &mut bool) {
    if std::mem::replace(done, true) {
        return;
    }
    window.set_decorations(true);
    window.set_resizable(true);
    window.set_inner_size(APP_SIZE);
    center(window, APP_SIZE);
    window.set_visible(true);
    window.set_focus();
    mark("app window shown");
}

fn send_after(proxy: &EventLoopProxy<UserEvent>, delay: Duration, ev: UserEvent) {
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let _ = proxy.send_event(ev);
    });
}

/// The local server once started: its origin and handle.
type Started = Arc<OnceLock<(String, Arc<ServerHandle>)>>;

fn data_dir() -> PathBuf {
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("fastobsidian")
}

/// Open a web link or a file with the system's default application.
fn open_with_system(target: &str) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
        let (op, t) = (wide("open"), wide(target));
        unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                t.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
        }
    }
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(target).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(target).spawn();
}

/// Links that leave the app: attachments open in their own app, web and mail links in the system's handler.
fn open_external(url: &str, origin: &str, server: &ServerHandle) {
    if let Some(rest) = url.strip_prefix(origin).and_then(|r| r.strip_prefix("vault/")) {
        let rel = rest.split(['?', '#']).next().unwrap_or("");
        if let Some((file, _)) = media_file(&server.vault(), rel) {
            open_with_system(&file.to_string_lossy());
        }
    } else if ["http://", "https://", "mailto:"].iter().any(|p| url.to_ascii_lowercase().starts_with(p)) {
        open_with_system(url);
    }
}

fn picker(proxy: &EventLoopProxy<UserEvent>) -> FolderPicker {
    // the dialog must run on the main thread (macOS requires it), so ask the event loop and wait
    let proxy = Mutex::new(proxy.clone());
    Box::new(move || {
        let (tx, rx) = mpsc::channel();
        proxy.lock().unwrap().send_event(UserEvent::PickFolder(tx)).ok()?;
        rx.recv().ok().flatten()
    })
}

fn vault_title(vault: &Path) -> String {
    let name =
        vault.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| vault.display().to_string());
    format!("{name} · FastObsidian")
}

fn js(s: &str) -> String {
    serde_json::Value::from(s).to_string()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

/// Background start-up: index the vault, read its theme and plugin list, start the server.
/// Each step is reported to the splash page.
fn start(vault: PathBuf, theme: bool, watch: f64, cache: bool, proxy: EventLoopProxy<UserEvent>, started: Started) {
    let say = |script: String| {
        let _ = proxy.send_event(UserEvent::Splash(script));
    };
    let step = |id: &str, text: &str, state: &str| say(format!("step('{id}', {}, '{state}')", js(text)));

    let t0 = std::time::Instant::now();
    let opts = |port| Options { port, theme, watch, native: true, picker: Some(picker(&proxy)), cache };
    // one splash update per percent, not per note
    let last = std::cell::Cell::new(usize::MAX);
    let progress = |done: usize, total: usize| {
        let pct = (done * 100).checked_div(total).unwrap_or(100);
        if last.replace(pct) != pct {
            say(format!("progress({done}, {total})"));
        }
    };
    let bind = |port| Server::bind_with_progress(&vault, opts(port), &progress);
    let server = match bind(PREFERRED_PORT).or_else(|_| bind(0)) {
        Ok(s) => s,
        Err(e) => {
            step("vault", "Could not open the vault", "fail");
            return say(format!("fail({})", js(&format!("cannot start the local server: {e}"))));
        }
    };
    mark("vault indexed, server listening");
    let indexed = format!("{} indexed in {:.1}s", plural(server.note_count(), "note"), t0.elapsed().as_secs_f64());
    step("vault", &indexed, "ok");

    if theme {
        step("theme", "Loading theme…", "run");
        let obs = obsidian_css(&vault);
        let mut text = if obs.theme.is_empty() { "Default theme".to_string() } else { format!("Theme: {}", obs.theme) };
        if !obs.snippets.is_empty() {
            text += &format!(" · {}", plural(obs.snippets.len(), "snippet"));
        }
        step("theme", &text, "ok");
    } else {
        step("theme", "Obsidian theme off (start with --theme)", "skip");
    }

    match enabled_plugins(&vault).len() {
        0 => step("plugins", "No community plugins", "skip"),
        n => step("plugins", &format!("{} enabled in Obsidian (not run here)", plural(n, "community plugin")), "skip"),
    }

    let url = server.url();
    let handle = server.spawn();
    {
        // serialize the vault data while the web view loads the page, which then asks for it
        let h = handle.clone();
        std::thread::spawn(move || {
            h.warm();
            mark("vault data ready");
        });
    }
    let _ = started.set((url.clone(), handle));
    say("done()".into());
    let _ = proxy.send_event(UserEvent::Ready(url));
}

pub fn run(vault: Option<PathBuf>, theme: bool, watch: f64, cache: bool) -> Result<(), String> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    mark("event loop ready");
    let proxy = event_loop.create_proxy();
    let vault = match vault {
        Some(v) => v,
        None => rfd::FileDialog::new().set_title("Open an Obsidian vault").pick_folder().ok_or("no folder chosen")?,
    };

    let started: Started = Arc::default();
    // index the vault from the start, while the window and web view are created: the two do not depend on each other, and creating a web view
    // can take a while (or, with a broken WebKitGTK sandbox, never finish)
    {
        let (v, p, s) = (vault.clone(), proxy.clone(), started.clone());
        std::thread::Builder::new()
            .name("startup".into())
            .spawn(move || start(v, theme, watch, cache, p, s))
            .map_err(|e| e.to_string())?;
    }
    send_after(&proxy, SPLASH_DELAY, UserEvent::ShowSplash);

    // starts hidden, as a small borderless splash that appears only if loading is slow;
    // grows into the app window once the vault is loaded
    let window = WindowBuilder::new()
        .with_title(vault_title(&vault))
        .with_visible(false)
        .with_window_icon(app_icon())
        .with_inner_size(SPLASH_SIZE)
        .with_decorations(false)
        .with_resizable(false)
        .build(&event_loop)
        .map_err(|e| e.to_string())?;
    center(&window, SPLASH_SIZE);
    mark("window created");

    let mut context = WebContext::new(Some(data_dir()));
    let (st1, st2) = (started.clone(), started.clone());
    let (p_ipc, p_title) = (proxy.clone(), proxy.clone());
    let splash = SPLASH
        .replace("__TITLE__", &html_escape(&vault_title(&vault)))
        .replace("__PATH__", &html_escape(&vault.display().to_string()).replace('"', "&quot;"))
        .replace(
            "__VAULT__",
            &html_escape(
                &vault.file_name().map_or_else(|| vault.display().to_string(), |n| n.to_string_lossy().into()),
            ),
        );
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_html(splash)
        .with_devtools(cfg!(debug_assertions))
        .with_new_window_req_handler(move |u, _| {
            if let Some((origin, server)) = st1.get() {
                open_external(&u, origin, server);
            }
            NewWindowResponse::Deny
        })
        .with_navigation_handler(move |u| {
            let Some((origin, server)) = st2.get() else {
                return true; // the splash page, before the server is up
            };
            if u.starts_with(origin.as_str()) || u.starts_with("about:") {
                return true;
            }
            open_external(&u, origin, server);
            false
        })
        .with_ipc_handler(move |req| {
            let body = req.body();
            if let Some(state) = body.strip_prefix("close:") {
                let _ = p_ipc.send_event(UserEvent::CloseAnswer { dirty: state == "dirty" });
            } else if body == "splash:ready" {
                let _ = p_ipc.send_event(UserEvent::SplashReady);
            } else if body == "app:ready" {
                let _ = p_ipc.send_event(UserEvent::AppReady);
            } else if body == "splash:close" {
                let _ = p_ipc.send_event(UserEvent::CloseAnswer { dirty: false });
            }
        })
        .with_document_title_changed_handler(move |t| {
            let _ = p_title.send_event(UserEvent::Title(t));
        });

    #[cfg(any(windows, target_os = "macos"))]
    let webview = builder.build(&window).map_err(|e| e.to_string())?;
    #[cfg(not(any(windows, target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().unwrap()).map_err(|e| e.to_string())?
    };
    mark("web view created");

    // splash scripts that arrive before the splash page has loaded wait here
    let mut pending: Option<Vec<String>> = Some(vec![]);
    let mut app_shown = false;
    let mut app_visible = false;
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let _ = &context; // keep the webview's data context alive as long as the window
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if !app_shown {
                    *flow = ControlFlow::Exit;
                    return;
                }
                // ask the page whether the editor has unsaved text; it answers through IPC
                let asked = webview.evaluate_script(
                    "window.ipc.postMessage('close:' + (typeof ED !== 'undefined' && ED.dirty ? 'dirty' : 'clean'))",
                );
                if asked.is_err() {
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::SplashReady) if !app_shown => {
                for script in pending.take().unwrap_or_default() {
                    let _ = webview.evaluate_script(&script);
                }
            }
            Event::UserEvent(UserEvent::Splash(script)) if !app_shown => match &mut pending {
                Some(queue) => queue.push(script),
                None => {
                    let _ = webview.evaluate_script(&script);
                }
            },
            Event::UserEvent(UserEvent::Ready(url)) => {
                app_shown = true;
                let _ = webview.load_url(&url);
                // a visible splash stays as it is until the app page has drawn itself
                send_after(&proxy, SHOW_FALLBACK, UserEvent::Show);
            }
            Event::UserEvent(UserEvent::ShowSplash) if !app_shown => {
                window.set_visible(true);
                window.set_focus(); // a window shown later must come to the front itself
            }
            Event::UserEvent(UserEvent::AppReady) if app_shown => {
                mark("app page drawn");
                show_app(&window, &mut app_visible)
            }
            Event::UserEvent(UserEvent::Show) if app_shown => show_app(&window, &mut app_visible),
            Event::UserEvent(UserEvent::CloseAnswer { dirty }) => {
                let close = !dirty
                    || rfd::MessageDialog::new()
                        .set_title("fastobsidian")
                        .set_description("This note has unsaved changes. Close anyway?")
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .set_level(rfd::MessageLevel::Warning)
                        .show()
                        == rfd::MessageDialogResult::Yes;
                if close {
                    *flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::PickFolder(tx)) => {
                let _ = tx.send(rfd::FileDialog::new().set_title("Open an Obsidian vault").pick_folder());
            }
            Event::UserEvent(UserEvent::Title(t)) => {
                window.set_title(if t.trim().is_empty() { "fastobsidian" } else { &t });
            }
            _ => {}
        }
    });
}
