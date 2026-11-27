//! Vault parsing, the incremental index and ranked search.
use fancy_regex::Regex;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Notes larger than this are left out of the index and refused by read and write:
/// a huge .md must not eat all memory. Changeable at runtime (tests lower it).
pub static MAX_NOTE_BYTES: AtomicUsize = AtomicUsize::new(32 * 1024 * 1024);

pub fn max_note_bytes() -> u64 {
    MAX_NOTE_BYTES.load(Ordering::Relaxed) as u64
}

pub const MEDIA_EXT: &[&str] =
    &["png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "pdf", "mp3", "mp4", "wav", "ogg", "webm", "mov", "m4a"];

fn is_asset_ext(ext: &str) -> bool {
    MEDIA_EXT.contains(&ext) || ext == "canvas" || ext == "base"
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        pub fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new($pat).expect($pat))
        }
    };
}

re!(fm_re, r"(?s)\A---[ \t]*\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|\z)");
// [text](note.md#h) or [text](<my note.md#h>) -- images (![..]) excluded
re!(mdlink_re, r#"(?i)(?<!!)\[[^\]]*\]\((?:<([^>]+?\.md)(?:#[^>]*)?>|([^)\s#]+?\.md)(?:#[^)\s]*)?)(?:\s+"[^"]*")?\)"#);
re!(scheme_re, r"(?i)\A[a-z][a-z0-9+.\-]*:");
re!(ws_re, r"\s+");
re!(fm_item_re, r"\A\s*-\s+(.*)");
re!(fm_kv_re, r"\A([\w-]+):\s*(.*)");
re!(split_re, r"[,\s]+");

// Note parsing runs on every note, so it uses the linear-time `regex` engine. Look-behinds and the
// fence backreference, which push fancy-regex onto its slow backtracking engine, are rewritten.
macro_rules! fast_re {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static regex::Regex {
            static R: OnceLock<regex::Regex> = OnceLock::new();
            R.get_or_init(|| regex::Regex::new($pat).expect($pat))
        }
    };
}

fast_re!(fence_fast, r"(?s)```.*?```|~~~.*?~~~"); // = (```|~~~).*?\1
fast_re!(inline_code_fast, r"`[^`\n]*`");
fast_re!(wiki_fast, r"(!?)\[\[([^\]|#]*)(#[^\]|]*)?(?:\|[^\]]*)?\]\]");
fast_re!(tag_fast, r"#([A-Za-z_][\w/-]*)"); // + not after [\w/&#]
fast_re!(tag_before, r"\A[\w/&#]\z");
fast_re!(mdlink_fast, r#"(?i)\[[^\]]*\]\((?:<([^>]+?\.md)(?:#[^>]*)?>|([^)\s#]+?\.md)(?:#[^)\s]*)?)(?:\s+"[^"]*")?\)"#); // + not after !
fast_re!(word_fast, r"\w+");

/// All captures of `re` in `text`, skipping matches whose preceding character satisfies `bad`:
/// a negative one-character look-behind. A skipped match resumes one character later, as a regex would.
fn captures_not_after<'t>(re: &regex::Regex, text: &'t str, bad: impl Fn(&str) -> bool) -> Vec<regex::Captures<'t>> {
    let (mut out, mut at) = (vec![], 0);
    while at <= text.len() {
        let Some(c) = re.captures_at(text, at) else { break };
        let m = c.get(0).unwrap();
        let prev = text[..m.start()].chars().next_back();
        if prev.is_some_and(|ch| bad(&text[m.start() - ch.len_utf8()..m.start()])) {
            at = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
        } else {
            at = m.end().max(m.start() + 1);
            out.push(c);
        }
    }
    out
}

/// Percent-decoding like Python's `urllib.parse.unquote` (invalid UTF-8 becomes U+FFFD).
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Universal newlines, as Python's text mode reads files.
pub fn normalize_newlines(s: &str) -> String {
    if s.contains('\r') {
        s.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        s.to_string()
    }
}

fn strip_quotes(s: &str) -> String {
    s.trim().trim_matches(|c| c == '\'' || c == '"').to_string()
}

/// Minimal YAML frontmatter: `key: value`, `key: [a, b]`, and `- item` lists. Returns (fields, body).
pub fn parse_frontmatter(text: &str) -> (Map<String, Value>, &str) {
    let mut fm = Map::new();
    if !text.starts_with("---") {
        return (fm, text);
    }
    let Ok(Some(m)) = fm_re().captures(text) else { return (fm, text) };
    let mut key: Option<String> = None;
    for line in m.get(1).unwrap().as_str().lines() {
        let item = fm_item_re().captures(line).ok().flatten();
        if let Ok(Some(kv)) = fm_kv_re().captures(line) {
            let k = kv[1].to_lowercase();
            let val = kv[2].trim();
            let v = if val.starts_with('[') && val.ends_with(']') && val.len() >= 2 {
                Value::Array(
                    val[1..val.len() - 1]
                        .split(',')
                        .filter(|v| !v.trim().is_empty())
                        .map(|v| Value::String(strip_quotes(v)))
                        .collect(),
                )
            } else if !val.is_empty() && matches!(k.as_str(), "tags" | "tag" | "aliases") {
                Value::Array(
                    split_re()
                        .split(val)
                        .filter_map(|v| v.ok())
                        .filter(|v| !v.trim().is_empty())
                        .map(|v| Value::String(strip_quotes(v)))
                        .collect(),
                )
            } else if val.is_empty() {
                Value::Array(vec![])
            } else {
                Value::String(val.to_string())
            };
            fm.insert(k.clone(), v);
            key = Some(k);
        } else if let (Some(item), Some(k)) = (item, &key) {
            if let Some(Value::Array(a)) = fm.get_mut(k) {
                a.push(Value::String(strip_quotes(&item[1])));
            }
        }
    }
    let end = m.get(0).unwrap().end();
    (fm, &text[end..])
}

fn value_list(v: Option<&Value>) -> Vec<String> {
    match v {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.iter().map(value_str).collect(),
        Some(other) => vec![value_str(other)],
    }
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(_) => true,
    }
}

#[derive(Debug, Clone)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub folder: String,
    pub content: String,
    pub lc: String,
    pub mtime: u64,
    pub rev: String,
    pub words: usize,
    pub tags: Vec<String>,
    pub props: Map<String, Value>,
    pub wiki: Vec<String>,
    pub mdl: Vec<String>,
}

impl Note {
    pub fn aliases(&self) -> Vec<String> {
        let v = if truthy(self.props.get("aliases")) { self.props.get("aliases") } else { self.props.get("alias") };
        value_list(v).into_iter().filter(|a| !a.trim().is_empty()).collect()
    }
}

/// Parse one note. Link *targets* are extracted here; resolving them needs the whole vault (see VaultIndex).
pub fn parse_note(rel: &str, mtime_ns: u64, text: &str) -> Note {
    let id = rel[..rel.len() - 3].to_string();
    let (mut fm, body) = parse_frontmatter(text);
    let no_fence = fence_fast().replace_all(body, "");
    let stripped = inline_code_fast().replace_all(&no_fence, "").into_owned();
    let mut tags = HashSet::new();
    for k in ["tags", "tag"] {
        for t in value_list(fm.get(k)) {
            if !t.is_empty() {
                tags.insert(t.trim_start_matches('#').to_string());
            }
        }
    }
    for c in captures_not_after(tag_fast(), &stripped, |p| tag_before().is_match(p)) {
        tags.insert(c[1].to_string());
    }
    let mut tags: Vec<String> = tags.into_iter().collect();
    tags.sort();
    fm.remove("tags");
    fm.remove("tag");
    let content = body.trim().to_string();
    let (folder, title) = match id.rsplit_once('/') {
        Some((f, t)) => (f.to_string(), t.to_string()),
        None => (String::new(), id.clone()),
    };
    Note {
        title,
        folder,
        lc: content.to_lowercase(),
        content,
        mtime: mtime_ns / 1_000_000_000,
        rev: mtime_ns.to_string(), // exact change marker for the browser's content cache (a string: ns overflow JS numbers)
        words: word_fast().find_iter(body).count(),
        tags,
        props: fm,
        wiki: wiki_fast().captures_iter(&stripped).map(|c| c[2].trim().to_string()).filter(|t| !t.is_empty()).collect(),
        mdl: captures_not_after(mdlink_fast(), &stripped, |p| p == "!")
            .into_iter()
            .map(|c| c.get(1).or(c.get(2)).unwrap().as_str().to_string())
            .collect(),
        id,
    }
}

/// Resolve a markdown-link target (relative to the note, or vault-root) to a note id.
pub fn resolve_md(raw: &str, from_id: &str, lookup: &Map<String, Value>) -> Option<String> {
    if scheme_re().is_match(raw).unwrap_or(false) {
        return None;
    }
    let un = unquote(raw);
    let target: String = {
        let n = un.chars().count();
        un.chars().take(n.saturating_sub(3)).collect()
    };
    let mut parts: Vec<&str> = if target.starts_with('/') {
        vec![]
    } else {
        match from_id.rsplit_once('/') {
            Some((dir, _)) => dir.split('/').collect(),
            None => vec![],
        }
    };
    let rest = target.trim_start_matches('/');
    for seg in rest.split('/') {
        if seg == ".." {
            parts.pop();
        } else if !seg.is_empty() && seg != "." {
            parts.push(seg);
        }
    }
    let get = |k: String| lookup.get(&k).and_then(|v| v.as_str()).map(str::to_string);
    get(parts.join("/").to_lowercase()).or_else(|| get(rest.to_lowercase()))
}

pub fn file_mtime_ns(md: &fs::Metadata) -> u64 {
    md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

/// One .md file found by [`walk_vault`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdFile {
    pub rel: String,
    pub path: PathBuf,
    pub mtime_ns: u64,
    pub size: u64,
}

/// (md, media): md sorted by rel path, media = rel paths.
/// Skips dot-folders, symlinks/junctions (files and folders) and notes over MAX_NOTE_BYTES.
pub fn walk_vault(vault: &Path) -> (Vec<MdFile>, Vec<String>) {
    let (mut md, mut media) = (vec![], vec![]);
    let mut stack = vec![(vault.to_path_buf(), String::new())];
    let max = max_note_bytes();
    while let Some((path, rel)) = stack.pop() {
        let Ok(it) = fs::read_dir(&path) else { continue };
        for e in it.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            // file_type() does not follow links; on Windows junctions count as symlinks too
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push((e.path(), r));
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let ext = match name.rsplit_once('.') {
                Some((_, x)) => x.to_lowercase(),
                None => String::new(),
            };
            if ext == "md" {
                let Ok(m) = e.metadata() else { continue };
                if m.len() <= max {
                    md.push(MdFile { rel: r, path: e.path(), mtime_ns: file_mtime_ns(&m), size: m.len() });
                }
            } else if MEDIA_EXT.contains(&ext.as_str()) {
                media.push(r);
            }
        }
    }
    md.sort_by(|a, b| a.rel.cmp(&b.rel));
    (md, media)
}

/// Read and parse `md[i]` for each i in `todo` on all cores. `progress(n)` is called on this thread
/// with the number parsed so far. Unreadable files map to None.
fn parse_files(md: &[MdFile], todo: &[usize], progress: &dyn Fn(usize)) -> HashMap<usize, Option<Arc<Note>>> {
    let parse = |i: usize| {
        let f = &md[i];
        let bytes = read_known(&f.path, f.size).ok()?;
        Some(Arc::new(parse_note(&f.rel, f.mtime_ns, &normalize_newlines(&String::from_utf8_lossy(&bytes)))))
    };
    if todo.len() < 64 {
        return todo.iter().map(|&i| (i, parse(i))).collect();
    }
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let (next, done) = (AtomicUsize::new(0), AtomicUsize::new(0));
    std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = vec![];
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&i) = todo.get(k) else { break out };
                        out.push((i, parse(i)));
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                })
            })
            .collect();
        while !workers.iter().all(|w| w.is_finished()) {
            progress(done.load(Ordering::Relaxed));
            std::thread::sleep(Duration::from_millis(20));
        }
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    })
}

/// `f` over `items` on all cores, results in input order. Small inputs stay on this thread.
pub(crate) fn map_parallel<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    if items.len() < 256 || threads == 1 {
        return items.iter().map(f).collect();
    }
    let chunk = items.len().div_ceil(threads * 4); // a few chunks per thread evens out the load
    let next = AtomicUsize::new(0);
    let chunks: Vec<&[T]> = items.chunks(chunk).collect();
    let mut parts: Vec<(usize, Vec<R>)> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut out = vec![];
                    loop {
                        let k = next.fetch_add(1, Ordering::Relaxed);
                        let Some(c) = chunks.get(k) else { break out };
                        out.push((k, c.iter().map(&f).collect::<Vec<R>>()));
                    }
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
    });
    parts.sort_unstable_by_key(|(k, _)| *k);
    parts.into_iter().flat_map(|(_, v)| v).collect()
}

/// One note's entry for the browser, plus the link targets that resolve to nothing (in order).
fn resolve_links(p: &Note, lookup: &Map<String, Value>) -> (Value, Vec<String>) {
    let get = |k: &str| lookup.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let (mut links, mut missing_out, mut seen) = (vec![], vec![], HashSet::new());
    let mut link = |dest: Option<String>, missing: Option<String>| {
        if let Some(d) = dest {
            if d != p.id && seen.insert(d.clone()) {
                links.push(d);
            }
        } else if let Some(m) = missing.filter(|m| !m.is_empty()) {
            missing_out.push(m);
        }
    };
    for target in &p.wiki {
        let key = strip_md_suffix(target).to_lowercase();
        let ext = key.rsplit_once('.').map(|(_, x)| x).unwrap_or("");
        let missing = if is_asset_ext(ext) { None } else { Some(target.clone()) };
        link(get(&key), missing);
    }
    for raw in &p.mdl {
        if scheme_re().is_match(raw).unwrap_or(false) {
            continue; // http:, mailto:, ... never resolve and are never "missing"
        }
        let base = unquote(raw);
        let base = base.rsplit('/').next().unwrap_or("");
        let missing: String = base.chars().take(base.chars().count().saturating_sub(3)).collect();
        link(resolve_md(raw, &p.id, lookup), Some(missing));
    }
    let note = json!({
        "id": p.id, "title": p.title, "folder": p.folder, "mtime": p.mtime, "rev": p.rev,
        "words": p.words, "tags": p.tags, "props": p.props, "links": links,
    });
    (note, missing_out)
}

/// Read a whole file whose size is already known from the walk: skips the size query `fs::read` makes.
fn read_known(path: &Path, size: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(size as usize + 1);
    fs::File::open(path)?.read_to_end(&mut buf)?;
    Ok(buf)
}

/// The resolved vault: what the browser gets (without note content), plus the link lookup.
#[derive(Debug)]
pub struct Resolved {
    pub vault_name: String,
    pub generated: String,
    pub notes: Vec<Value>,
    pub lookup: Map<String, Value>,
    pub assets: Map<String, Value>,
    pub unresolved: Vec<(String, Vec<String>)>,
}

impl Resolved {
    pub fn to_json(&self) -> Value {
        json!({
            "vault": self.vault_name, "generated": self.generated, "notes": self.notes,
            "lookup": self.lookup, "assets": self.assets,
            "unresolved": self.unresolved.iter().map(|(t, s)| json!({"target": t, "sources": s})).collect::<Vec<_>>(),
        })
    }

    pub fn note_ids(&self) -> impl Iterator<Item = &str> {
        self.notes.iter().filter_map(|n| n["id"].as_str())
    }
}

/// What the vault looked like at the last walk: (rel, mtime_ns, size) per note, plus the media files.
type Signature = (Vec<(String, u64, u64)>, Vec<String>);

#[derive(Default)]
struct Inner {
    files: HashMap<String, (u64, u64, Arc<Note>)>,
    sig: Option<Signature>,
    result: Option<Arc<Resolved>>,
    json: Option<Arc<String>>,
    /// id -> parsed note (with content), in vault order
    notes: Vec<Arc<Note>>,
    by_id: HashMap<String, usize>,
    checked: Option<Instant>,
    stale: bool,
    /// the on-disk cache was loaded (or tried) already
    cache_tried: bool,
    /// changes not yet written to the on-disk cache, and when it was last written
    cache_unsaved: bool,
    cache_saved: Option<Instant>,
}

/// The on-disk cache is rewritten at most this often (a missed save only costs re-reading a few notes).
const CACHE_SAVE_EVERY: Duration = Duration::from_secs(30);

/// Incremental vault index: re-reads only files whose mtime/size changed; caches the resolved result.
pub struct VaultIndex {
    pub vault: PathBuf,
    /// on-disk cache of parsed notes (see crate::cache); None = no cache
    cache: Option<PathBuf>,
    inner: Mutex<Inner>,
    version: AtomicUsize,
}

pub const MIN_RECHECK: Duration = Duration::from_secs(2);

impl VaultIndex {
    pub fn new(vault: &Path) -> Self {
        VaultIndex {
            vault: vault.to_path_buf(),
            cache: None,
            inner: Mutex::new(Inner { stale: true, ..Default::default() }),
            version: AtomicUsize::new(0),
        }
    }

    /// An index that starts from, and keeps updating, the vault's on-disk cache of parsed notes.
    pub fn with_cache(vault: &Path) -> Self {
        VaultIndex { cache: Some(crate::cache::cache_file(vault)), ..Self::new(vault) }
    }

    /// Bumped every time the vault content changes (the watcher pushes it to browsers).
    pub fn version(&self) -> usize {
        self.version.load(Ordering::SeqCst)
    }

    /// Call after writing through the server so the next build re-walks immediately.
    pub fn invalidate(&self) {
        self.inner.lock().unwrap().stale = true;
    }

    /// Current index. Without `force`, a walk happens at most every MIN_RECHECK (or after invalidate).
    pub fn build(&self, force: bool) -> Arc<Resolved> {
        self.build_with_progress(force, &|_, _| {})
    }

    /// [`build`](Self::build), calling `progress(done, total)` while note files are read.
    pub fn build_with_progress(&self, force: bool, progress: &dyn Fn(usize, usize)) -> Arc<Resolved> {
        let mut g = self.inner.lock().unwrap();
        if let (false, Some(r), false, Some(t)) = (force, &g.result, g.stale, g.checked) {
            if t.elapsed() < MIN_RECHECK {
                return r.clone();
            }
        }
        let (md, mut media) = walk_vault(&self.vault);
        g.checked = Some(Instant::now());
        g.stale = false;
        media.sort();
        let sig = (md.iter().map(|f| (f.rel.clone(), f.mtime_ns, f.size)).collect::<Vec<_>>(), media.clone());
        if let (Some(old), Some(r)) = (&g.sig, &g.result) {
            if *old == sig {
                return r.clone();
            }
        }
        if !g.cache_tried {
            g.cache_tried = true;
            if let (Some(file), true) = (&self.cache, g.files.is_empty()) {
                if let Some(files) = crate::cache::load(file, &self.vault) {
                    g.files = files;
                }
            }
        }
        let unchanged = |f: &MdFile| g.files.get(&f.rel).filter(|o| o.0 == f.mtime_ns && o.1 == f.size).cloned();
        let todo: Vec<usize> = (0..md.len()).filter(|&i| unchanged(&md[i]).is_none()).collect();
        let parsed = parse_files(&md, &todo, &|n| progress(md.len() - todo.len() + n, md.len()));
        let mut files = HashMap::new();
        let mut notes = vec![];
        for (i, f) in md.iter().enumerate() {
            let entry = match parsed.get(&i) {
                Some(Some(n)) => (f.mtime_ns, f.size, n.clone()),
                Some(None) => continue, // unreadable
                None => match unchanged(f) {
                    Some(old) => old,
                    None => continue,
                },
            };
            notes.push(entry.2.clone());
            files.insert(f.rel.clone(), entry);
        }
        progress(md.len(), md.len());
        g.by_id = notes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
        g.cache_unsaved |= !todo.is_empty() || files.len() != g.files.len();
        g.files = files;
        if let Some(file) = &self.cache {
            if g.cache_unsaved && g.cache_saved.is_none_or(|t| t.elapsed() >= CACHE_SAVE_EVERY) {
                g.cache_unsaved = false;
                g.cache_saved = Some(Instant::now());
                let snap: Vec<_> = g.files.iter().map(|(r, (m, s, n))| (r.clone(), *m, *s, n.clone())).collect();
                let (file, vault) = (file.clone(), self.vault.clone());
                // in the background: startup and edits never wait for the write
                thread::spawn(move || {
                    if let Err(e) = crate::cache::save(&file, &vault, &snap) {
                        eprintln!("index cache not saved: {e}");
                    }
                });
            }
        }
        let res = Arc::new(self.resolve(&notes, &media));
        g.notes = notes;
        g.result = Some(res.clone());
        g.json = None;
        g.sig = Some(sig);
        self.version.fetch_add(1, Ordering::SeqCst);
        res
    }

    fn resolve(&self, parsed: &[Arc<Note>], media: &[String]) -> Resolved {
        let mut by_depth: Vec<(usize, &Arc<Note>)> = parsed.iter().map(|p| (p.id.matches('/').count(), p)).collect();
        by_depth.sort_unstable_by(|a, b| (a.0, &a.1.id).cmp(&(b.0, &b.1.id)));
        let by_depth: Vec<&Arc<Note>> = by_depth.into_iter().map(|(_, p)| p).collect();
        let mut lookup = Map::new();
        for p in &by_depth {
            let parts: Vec<&str> = p.id.split('/').collect();
            for i in 0..parts.len() {
                lookup.entry(parts[i..].join("/").to_lowercase()).or_insert_with(|| Value::String(p.id.clone()));
            }
        }
        let mut by_id: Vec<&Arc<Note>> = parsed.iter().collect();
        by_id.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        for p in &by_id {
            // aliases come last: real note names win
            for a in p.aliases() {
                lookup.entry(a.to_lowercase()).or_insert_with(|| Value::String(p.id.clone()));
            }
        }
        // each note's links resolve independently: do it on all cores, then merge in note order
        let per_note = map_parallel(&by_id, |p| resolve_links(p, &lookup));
        let mut unresolved: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        let mut notes = Vec::with_capacity(per_note.len());
        for (p, (note, missing)) in by_id.iter().zip(per_note) {
            for m in missing {
                let src = unresolved.entry(m).or_default();
                if src.last() != Some(&p.id) {
                    src.push(p.id.clone());
                }
            }
            notes.push(note);
        }
        let mut media_sorted: Vec<&String> = media.iter().collect();
        media_sorted.sort_by(|a, b| (a.matches('/').count(), a).cmp(&(b.matches('/').count(), b)));
        let mut assets = Map::new();
        for rel in media_sorted {
            let parts: Vec<&str> = rel.split('/').collect();
            for i in 0..parts.len() {
                assets.entry(parts[i..].join("/").to_lowercase()).or_insert_with(|| Value::String(rel.clone()));
            }
        }
        let name = self.vault.file_name().map(|n| n.to_string_lossy().into_owned());
        Resolved {
            vault_name: name.filter(|n| !n.is_empty()).unwrap_or_else(|| self.vault.display().to_string()),
            generated: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            notes,
            lookup,
            assets,
            unresolved: unresolved.into_iter().collect(),
        }
    }

    /// Serialized result, cached until the vault changes.
    pub fn json(&self) -> Arc<String> {
        let res = self.build(false);
        let mut g = self.inner.lock().unwrap();
        if let (Some(j), Some(r)) = (&g.json, &g.result) {
            if Arc::ptr_eq(r, &res) {
                return j.clone();
            }
        }
        let j = Arc::new(res.to_json().to_string());
        if g.result.as_ref().is_some_and(|r| Arc::ptr_eq(r, &res)) {
            g.json = Some(j.clone());
        }
        j
    }

    /// Parsed notes (with content), in vault order.
    pub fn notes(&self) -> Vec<Arc<Note>> {
        self.build(false);
        self.inner.lock().unwrap().notes.clone()
    }

    pub fn note(&self, id: &str) -> Option<Arc<Note>> {
        self.build(false);
        let g = self.inner.lock().unwrap();
        g.by_id.get(id).map(|&i| g.notes[i].clone())
    }

    pub fn contents(&self, ids: &[String]) -> Map<String, Value> {
        self.build(false);
        let g = self.inner.lock().unwrap();
        ids.iter()
            .filter_map(|i| g.by_id.get(i).map(|&k| (i.clone(), Value::String(g.notes[k].content.clone()))))
            .collect()
    }

    /// Ranked search: name > alias > tag > body. `tag` restricts to notes carrying that tag.
    pub fn search(&self, q: &str, limit: usize, tag: &str) -> Vec<Value> {
        let ql = q.trim().to_lowercase();
        let tag = tag.trim().to_lowercase();
        let tag = tag.trim_start_matches('#');
        let mut out: Vec<(i32, String, String)> = vec![];
        if ql.is_empty() && tag.is_empty() {
            return vec![];
        }
        for p in self.notes() {
            let tags: Vec<String> = p.tags.iter().map(|t| t.to_lowercase()).collect();
            if !tag.is_empty() && !tags.iter().any(|t| t == tag) {
                continue;
            }
            // char index of the first body hit (in the lowercased text, as Python's str.find)
            let hit = if ql.is_empty() { None } else { p.lc.find(&ql).map(|b| p.lc[..b].chars().count()) };
            let mut score = 0;
            if !ql.is_empty() {
                score = if p.title.to_lowercase() == ql {
                    120
                } else if p.id.to_lowercase().contains(&ql) {
                    100
                } else if p.aliases().iter().any(|a| a.to_lowercase().contains(&ql)) {
                    80
                } else if tags.iter().any(|t| t.contains(&ql)) {
                    60
                } else if hit.is_some() {
                    10
                } else {
                    0
                };
                if score == 0 {
                    continue;
                }
            }
            let snip: String = match hit {
                Some(i) => p.content.chars().skip(i.saturating_sub(40)).take(i + 80 - i.saturating_sub(40)).collect(),
                None => p.content.chars().take(120).collect(),
            };
            out.push((score, p.id.clone(), ws_re().replace_all(&snip, " ").into_owned()));
        }
        out.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        out.into_iter().take(limit).map(|(s, id, snip)| json!({"id": id, "snippet": snip, "score": s})).collect()
    }
}

pub fn strip_md_suffix(s: &str) -> &str {
    if s.len() >= 3 && s.is_char_boundary(s.len() - 3) && s[s.len() - 3..].eq_ignore_ascii_case(".md") {
        &s[..s.len() - 3]
    } else {
        s
    }
}

/// One-shot build (static mode): note content is embedded.
pub fn build_with_content(vault: &Path) -> Value {
    let idx = VaultIndex::new(vault);
    let res = idx.build(false);
    let mut data = res.to_json();
    let notes = idx.notes();
    let by_id: HashMap<&str, &Arc<Note>> = notes.iter().map(|n| (n.id.as_str(), n)).collect();
    if let Some(arr) = data["notes"].as_array_mut() {
        for n in arr {
            let id = n["id"].as_str().unwrap_or("").to_string();
            if let Some(p) = by_id.get(id.as_str()) {
                n["content"] = Value::String(p.content.clone());
            }
        }
    }
    data
}
