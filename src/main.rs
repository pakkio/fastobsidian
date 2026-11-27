//! Command line: the app window by default, or --serve / -o FILE / --mcp.
mod gui;
mod icon;

use fastobsidian::server::{expand_path, Options, Server};
use fastobsidian::{mcp, page};
use std::path::PathBuf;
use std::process::ExitCode;

const HELP: &str = "\
fastobsidian - fast explorer and editor for an Obsidian vault

USAGE:
    fastobsidian [VAULT]                 open the vault in the app window (no VAULT: choose a folder)
    fastobsidian VAULT --serve           serve it at http://127.0.0.1:8765/ for any browser
    fastobsidian VAULT -o explorer.html  write a self-contained read-only page
    fastobsidian VAULT --mcp             MCP server on stdio, for AI agents

OPTIONS:
    --theme            use the vault's Obsidian theme and enabled CSS snippets (.obsidian/appearance.json)
    --watch SECONDS    how often to check the vault for outside changes while a page is open (default 1, 0 = off)
    --no-cache         do not read or write the index cache (see the README)
    --port N           port for --serve (default 8765)
    -o, --out FILE     static output file
    -h, --help         this help
    -V, --version      version
";

struct Args {
    vault: Option<String>,
    serve: bool,
    mcp: bool,
    out: Option<String>,
    port: u16,
    theme: bool,
    watch: f64,
    cache: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut a =
        Args { vault: None, serve: false, mcp: false, out: None, port: 8765, theme: false, watch: 1.0, cache: true };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("fastobsidian {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--serve" => a.serve = true,
            "--mcp" => a.mcp = true,
            "--theme" => a.theme = true,
            "--no-cache" => a.cache = false,
            "--port" => a.port = value("--port")?.parse().map_err(|_| "--port must be a number")?,
            "--watch" => {
                a.watch = value("--watch")?.parse().map_err(|_| "--watch must be a number of seconds")?;
                if !(a.watch >= 0.0 && a.watch.is_finite()) {
                    return Err("--watch must be 0 or more".into());
                }
            }
            "-o" | "--out" => a.out = Some(value("-o")?),
            s if s.starts_with('-') => return Err(format!("unknown option: {s} (see --help)")),
            s if a.vault.is_none() => a.vault = Some(s.to_string()),
            s => return Err(format!("unexpected argument: {s}")),
        }
    }
    Ok(a)
}

/// Started by double-click on Windows, the process owns a console window of its own: hide it for the app window.
fn hide_own_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
        let mut ids = [0u32; 2];
        if GetConsoleProcessList(ids.as_mut_ptr(), 2) == 1 {
            FreeConsole();
        }
    }
}

fn main() -> ExitCode {
    gui::mark("start");
    let a = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let vault = |v: &Option<String>| -> Result<PathBuf, String> {
        let p = expand_path(v.as_deref().unwrap_or("."));
        if p.is_dir() {
            Ok(p)
        } else {
            Err(format!("not a directory: {}", p.display()))
        }
    };
    let result = (|| -> Result<(), String> {
        if a.mcp {
            mcp::run(&vault(&a.vault)?);
        } else if let Some(out) = &a.out {
            let v = vault(&a.vault)?;
            let out = std::path::absolute(out).map_err(|e| e.to_string())?;
            std::fs::write(&out, page::static_page(&v, a.theme)).map_err(|e| format!("{}: {e}", out.display()))?;
            println!("static read-only explorer -> {}", out.display());
        } else if a.serve {
            let v = vault(&a.vault)?;
            let opts =
                Options { port: a.port, theme: a.theme, watch: a.watch, native: false, picker: None, cache: a.cache };
            let srv = Server::bind(&v, opts).map_err(|e| format!("cannot listen on 127.0.0.1:{}: {e}", a.port))?;
            println!("Serving {} at {}  (Ctrl+C to stop; deleted notes go to .trash)", v.display(), srv.url());
            srv.run();
        } else {
            let v = match &a.vault {
                Some(_) => Some(vault(&a.vault)?),
                None => None,
            };
            hide_own_console();
            gui::run(v, a.theme, a.watch, a.cache)?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
