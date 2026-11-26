//! Local HTTP server (127.0.0.1 only) behind the app window and `--serve`.
//!
//! Hand-rolled HTTP/1.1 with one thread per connection and `Connection: close`: the clients are
//! one local webview or browser, so this stays small and dependency-free.
use crate::ops::{create_note, no_links, read_note, rename_note, save_note, trash_note, Error};
use crate::page::live_page;
use crate::vault::{max_note_bytes, unquote, VaultIndex, MEDIA_EXT};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::thread;
use std::time::Duration;

/// Asks the user for a folder (native dialog); used by "open vault" inside the app window.
pub type FolderPicker = Box<dyn Fn() -> Option<PathBuf> + Send + Sync>;

pub struct Options {
    pub port: u16,
    /// apply the vault's Obsidian theme and CSS snippets
    pub theme: bool,
    /// seconds between vault checks while a page listens for changes (0 = off)
    pub watch: f64,
    /// running inside the app window
    pub native: bool,
    pub picker: Option<FolderPicker>,
}

struct Events {
    v: u64,
    clients: usize,
}

struct Shared {
    current: RwLock<(PathBuf, Arc<VaultIndex>)>,
    opts: Options,
    allowed_hosts: Vec<String>,
    events: (Mutex<Events>, Condvar),
}

pub struct Server {
    pub port: u16,
    shared: Arc<Shared>,
    listener: TcpListener,
}

impl Server {
    /// Index the vault and bind 127.0.0.1:`port` (0 = any free port). Call `spawn` or `run` to serve.
    pub fn bind(vault: &Path, opts: Options) -> io::Result<Server> {
        let listener = TcpListener::bind(("127.0.0.1", opts.port))?;
        let port = listener.local_addr()?.port();
        let idx = Arc::new(VaultIndex::new(vault));
        let t0 = std::time::Instant::now();
        let n = idx.build(false).notes.len();
        eprintln!("indexed {n} notes in {:.1}s", t0.elapsed().as_secs_f64());
        let shared = Arc::new(Shared {
            current: RwLock::new((vault.to_path_buf(), idx)),
            allowed_hosts: vec![format!("127.0.0.1:{port}"), format!("localhost:{port}")],
            opts,
            events: (Mutex::new(Events { v: 0, clients: 0 }), Condvar::new()),
        });
        if shared.opts.watch > 0.0 {
            let s = shared.clone();
            thread::Builder::new().name("vault-watcher".into()).spawn(move || watcher(s))?;
        }
        Ok(Server { port, shared, listener })
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    pub fn vault(&self) -> PathBuf {
        self.shared.current.read().unwrap().0.clone()
    }

    /// Serve in a background thread.
    pub fn spawn(self) -> Arc<ServerHandle> {
        let handle = Arc::new(ServerHandle { shared: self.shared.clone(), port: self.port });
        thread::spawn(move || self.run());
        handle
    }

    /// Serve on this thread, forever.
    pub fn run(self) {
        for stream in self.listener.incoming().flatten() {
            let s = self.shared.clone();
            thread::spawn(move || {
                let _ = handle_conn(stream, &s);
            });
        }
    }
}

/// What the app window needs from a running server.
pub struct ServerHandle {
    shared: Arc<Shared>,
    pub port: u16,
}

impl ServerHandle {
    pub fn vault(&self) -> PathBuf {
        self.shared.current.read().unwrap().0.clone()
    }
}

/// Re-check the vault while at least one page listens on /api/events; announce every change.
fn watcher(s: Arc<Shared>) {
    let key = |idx: &Arc<VaultIndex>| (Arc::as_ptr(idx) as usize, idx.version());
    let mut last = key(&s.current.read().unwrap().1);
    loop {
        thread::sleep(Duration::from_secs_f64(s.opts.watch));
        if s.events.0.lock().unwrap().clients == 0 {
            continue;
        }
        let idx = s.current.read().unwrap().1.clone();
        idx.build(true);
        let k = key(&idx);
        if k != last {
            last = k;
            let (m, cv) = &s.events;
            m.lock().unwrap().v += 1;
            cv.notify_all();
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.get(&k.to_ascii_lowercase()).map(String::as_str)
    }

    fn json(&self) -> Result<Value, Error> {
        if self.body.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&self.body).map_err(|e| Error::Invalid(format!("bad JSON: {e}")))
    }

    fn q(&self, k: &str) -> &str {
        self.query.get(k).map(String::as_str).unwrap_or("")
    }
}

fn parse_query(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let dec = |s: &str| unquote(&s.replace('+', " "));
            (!v.is_empty()).then(|| (dec(k), dec(v)))
        })
        .collect()
}

fn read_request(stream: &TcpStream) -> io::Result<Option<Request>> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut r = BufReader::new(stream);
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else { return Ok(None) };
    let (method, target) = (method.to_string(), target.to_string());
    let mut headers = HashMap::new();
    let mut total = 0;
    loop {
        let mut h = String::new();
        let n = r.read_line(&mut h)?;
        total += n;
        if n == 0 || h == "\r\n" || h == "\n" || total > 64 * 1024 {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: u64 = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > 4 * max_note_bytes() + (1 << 20) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "request too large"));
    }
    let mut body = vec![0; len as usize];
    r.read_exact(&mut body)?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Ok(Some(Request { method, path: path.to_string(), query: parse_query(query), headers, body }))
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        501 => "Not Implemented",
        _ => "Internal Server Error",
    }
}

fn send(stream: &mut TcpStream, code: u16, ctype: &str, body: &[u8]) -> io::Result<()> {
    let textual = ctype.starts_with("text/") || ctype == "application/json";
    let mut h = format!("HTTP/1.1 {code} {}\r\nContent-Type: {ctype}", reason(code));
    if textual {
        h.push_str("; charset=utf-8");
    } else {
        h.push_str("\r\nX-Content-Type-Options: nosniff");
    }
    if ctype == "image/svg+xml" {
        // an SVG opened directly must not run script in our origin
        h.push_str("\r\nContent-Security-Policy: sandbox; default-src 'none'; style-src 'unsafe-inline'");
    }
    h.push_str(&format!("\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len()));
    stream.write_all(h.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn send_json(stream: &mut TcpStream, code: u16, v: &Value) -> io::Result<()> {
    send(stream, code, "application/json", v.to_string().as_bytes())
}

fn mime(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// Map /vault/<rel> to an attachment file, or None. Only media files, no dot-folders, no links.
pub fn media_file(vault: &Path, rel_url: &str) -> Option<(PathBuf, &'static str)> {
    let rel = unquote(rel_url);
    let segs: Vec<&str> = rel.split('/').collect();
    if segs.iter().any(|s| s.is_empty() || s.starts_with('.') || s.contains('\\') || s.contains(':')) {
        return None;
    }
    let ext = rel.rsplit_once('.').map(|(_, x)| x.to_lowercase()).unwrap_or_default();
    if !MEDIA_EXT.contains(&ext.as_str()) {
        return None;
    }
    let mut f = vault.to_path_buf();
    segs.iter().for_each(|s| f.push(s));
    (no_links(vault, &f).is_ok() && f.is_file()).then(|| (f, mime(&ext)))
}

fn handle_conn(mut stream: TcpStream, s: &Shared) -> io::Result<()> {
    let Some(req) = read_request(&stream)? else { return Ok(()) };
    let write = !matches!(req.method.as_str(), "GET" | "HEAD");
    // DNS-rebinding protection
    if !req.header("host").is_some_and(|h| s.allowed_hosts.iter().any(|a| a == h)) {
        return send_json(&mut stream, 403, &json!({"error": "bad host"}));
    }
    if write {
        let origin_ok = match req.header("origin") {
            None => true,
            Some(o) => {
                let netloc = o.split_once("://").map(|(_, r)| r.split('/').next().unwrap_or("")).unwrap_or("");
                s.allowed_hosts.iter().any(|a| a == netloc)
            }
        };
        if req.header("x-explorer") != Some("1") || !origin_ok {
            return send_json(&mut stream, 403, &json!({"error": "forbidden"}));
        }
    }
    if req.method == "GET" && req.path == "/api/events" {
        return stream_events(stream, s);
    }
    let (vault, idx) = s.current.read().unwrap().clone();
    let res: Result<Response, Error> = route(&req, s, &vault, &idx);
    match res {
        Ok(Response::Json(v)) => send_json(&mut stream, 200, &v),
        Ok(Response::Raw(code, ctype, body)) => send(&mut stream, code, ctype, &body),
        Err(e) => {
            let code = match e {
                Error::Invalid(_) => 400,
                Error::NotFound => 404,
                Error::Conflict(_) => 409,
                Error::Io(_) => 500,
            };
            send_json(&mut stream, code, &json!({"error": e.to_string()}))
        }
    }
}

enum Response {
    Json(Value),
    Raw(u16, &'static str, Vec<u8>),
}

fn route(req: &Request, s: &Shared, vault: &Path, idx: &Arc<VaultIndex>) -> Result<Response, Error> {
    use Response::*;
    let p = req.path.as_str();
    Ok(match (req.method.as_str(), p) {
        ("GET", "/") => Raw(200, "text/html", live_page(idx, s.opts.theme, s.opts.native).into_bytes()),
        ("GET", "/api/data") => Raw(200, "application/json", idx.json().as_bytes().to_vec()),
        ("GET", "/api/vault") => Json(json!({"path": vault.display().to_string()})),
        ("GET", "/api/search") => Json(Value::Array(idx.search(req.q("q"), 300, req.q("tag")))),
        ("GET", "/api/note") => Json(read_note(vault, req.q("id"))?),
        ("GET", _) if p.starts_with("/vault/") => {
            let (f, ctype) = media_file(vault, &p["/vault/".len()..]).ok_or(Error::NotFound)?;
            Raw(200, ctype, std::fs::read(f)?)
        }
        ("PUT", "/api/note") => {
            let d = req.json()?;
            Json(save_note(
                vault,
                idx,
                d["id"].as_str().unwrap_or(""),
                d["raw"].as_str().unwrap_or(""),
                d.get("mtime"),
                false,
            )?)
        }
        ("POST", "/api/contents") => {
            // read-only; POST only to carry a long id list
            let d = req.json()?;
            let ids = d["ids"].as_array().filter(|a| a.len() <= 2000).ok_or(Error::Invalid("bad ids".into()))?;
            let ids: Vec<String> =
                ids.iter().map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).collect();
            Json(Value::Object(idx.contents(&ids)))
        }
        ("POST", "/api/note") => {
            let d = req.json()?;
            Json(create_note(vault, idx, d["id"].as_str().unwrap_or(""), d["raw"].as_str().unwrap_or(""))?)
        }
        ("POST", "/api/rename") => {
            let d = req.json()?;
            Json(rename_note(vault, idx, d["id"].as_str().unwrap_or(""), d["new"].as_str().unwrap_or(""))?)
        }
        ("POST", "/api/vault") => {
            let d = req.json()?;
            let path = d["path"].as_str().unwrap_or("").trim().trim_matches('"').to_string();
            Json(switch(s, &path)?)
        }
        ("POST", "/api/pick-vault") => match s.opts.picker.as_ref().and_then(|pick| pick()) {
            Some(path) => Json(switch(s, &path.to_string_lossy())?),
            None => Json(json!({"ok": false})),
        },
        ("DELETE", "/api/note") => Json(trash_note(vault, idx, req.q("id"))?),
        ("GET" | "PUT" | "POST" | "DELETE", _) => return Err(Error::NotFound),
        _ => Raw(501, "application/json", br#"{"error": "not implemented"}"#.to_vec()),
    })
}

/// "~/x" -> home/x; relative -> absolute.
pub fn expand_path(p: &str) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let pb =
        match (p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")).or(if p == "~" { Some("") } else { None }), home)
        {
            (Some(rest), Some(h)) => PathBuf::from(h).join(rest),
            _ => PathBuf::from(p),
        };
    std::path::absolute(&pb).unwrap_or(pb)
}

fn switch(s: &Shared, path: &str) -> Result<Value, Error> {
    let new = expand_path(path);
    if !new.is_dir() {
        return Err(Error::Invalid(format!("not a directory: {}", new.display())));
    }
    let idx = Arc::new(VaultIndex::new(&new));
    idx.build(false);
    *s.current.write().unwrap() = (new.clone(), idx);
    Ok(json!({"ok": true, "path": new.display().to_string()}))
}

/// Server-sent events: `hello` with the current version, then `change` whenever the vault changes.
fn stream_events(mut stream: TcpStream, s: &Shared) -> io::Result<()> {
    stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
    )?;
    let (m, cv) = &s.events;
    let mut seen = {
        let mut e = m.lock().unwrap();
        e.clients += 1;
        e.v
    };
    let res = (|| -> io::Result<()> {
        stream.write_all(format!("event: hello\ndata: {seen}\n\n").as_bytes())?;
        stream.flush()?;
        loop {
            let v = {
                let e = m.lock().unwrap();
                let (e, _) = cv.wait_timeout_while(e, Duration::from_secs(15), |e| e.v == seen).unwrap();
                e.v
            };
            if v != seen {
                seen = v;
                stream.write_all(format!("event: change\ndata: {v}\n\n").as_bytes())?;
            } else {
                stream.write_all(b": ping\n\n")?; // detects closed pages
            }
            stream.flush()?;
        }
    })();
    m.lock().unwrap().clients -= 1;
    res
}
