//! The app window: the system webview (WebView2 / WebKit) showing the local server's page.
use fastobsidian::server::{media_file, FolderPicker, Options, Server, ServerHandle};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::{NewWindowResponse, WebContext, WebViewBuilder};

/// Fixed so the page's origin, and with it the remembered light/dark choice, stays the same between launches.
const PREFERRED_PORT: u16 = 47615;

enum UserEvent {
    PickFolder(mpsc::Sender<Option<PathBuf>>),
    CloseAnswer { dirty: bool },
    Title(String),
}

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
    format!("{name} · fastobsidian")
}

pub fn run(vault: Option<PathBuf>, theme: bool, watch: f64) -> Result<(), String> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let vault = match vault {
        Some(v) => v,
        None => rfd::FileDialog::new().set_title("Open an Obsidian vault").pick_folder().ok_or("no folder chosen")?,
    };
    let opts = |port| Options { port, theme, watch, native: true, picker: Some(picker(&proxy)), cache: true };
    let server = Server::bind(&vault, opts(PREFERRED_PORT))
        .or_else(|_| Server::bind(&vault, opts(0)))
        .map_err(|e| format!("cannot start the local server: {e}"))?;
    let url = server.url();
    let server: Arc<ServerHandle> = server.spawn();

    let window = WindowBuilder::new()
        .with_title(vault_title(&vault))
        .with_inner_size(LogicalSize::new(1400.0, 900.0))
        .build(&event_loop)
        .map_err(|e| e.to_string())?;

    let mut context = WebContext::new(Some(data_dir()));
    let (o1, s1) = (url.clone(), server.clone());
    let (o2, s2) = (url.clone(), server.clone());
    let (p_ipc, p_title) = (proxy.clone(), proxy.clone());
    let builder = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(&url)
        .with_devtools(cfg!(debug_assertions))
        .with_new_window_req_handler(move |u, _| {
            open_external(&u, &o1, &s1);
            NewWindowResponse::Deny
        })
        .with_navigation_handler(move |u| {
            if u.starts_with(&o2) || u.starts_with("about:") {
                return true;
            }
            open_external(&u, &o2, &s2);
            false
        })
        .with_ipc_handler(move |req| {
            if let Some(state) = req.body().strip_prefix("close:") {
                let _ = p_ipc.send_event(UserEvent::CloseAnswer { dirty: state == "dirty" });
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

    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        let _ = &context; // keep the webview's data context alive as long as the window
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                // ask the page whether the editor has unsaved text; it answers through IPC
                let asked = webview.evaluate_script(
                    "window.ipc.postMessage('close:' + (typeof ED !== 'undefined' && ED.dirty ? 'dirty' : 'clean'))",
                );
                if asked.is_err() {
                    *flow = ControlFlow::Exit;
                }
            }
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
