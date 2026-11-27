//! On-disk cache of the parsed notes, so a restart re-reads only the notes that changed.
//!
//! One binary file per vault in the app's data folder. Each entry keeps the file's mtime and size; the
//! index trusts an entry only while both still match the file, so a stale cache costs time, never
//! correctness. A cache from another app version, another vault or with a damaged byte is ignored whole.
use crate::vault::{map_parallel, Note};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Bump whenever parsing or this format changes, so old caches are dropped.
const FORMAT: u32 = 1;
const MAGIC: &[u8; 6] = b"FOIDX\0";

/// One parsed note with the file's (mtime_ns, size) when it was read.
pub type Entry = (u64, u64, Arc<Note>);
/// The parsed notes of a vault, keyed by relative path.
pub type Files = HashMap<String, Entry>;

/// The app's per-user data folder (also holds the webview's data).
pub fn app_data_dir() -> PathBuf {
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support"))
    } else {
        env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache")))
    };
    base.unwrap_or_else(std::env::temp_dir).join("fastobsidian")
}

/// Where the cache of `vault` lives: named by a stable hash (FNV-1a) of its full path.
pub fn cache_file(vault: &Path) -> PathBuf {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in vault.to_string_lossy().as_bytes() {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    app_data_dir().join("index-cache").join(format!("{h:016x}.idx"))
}

fn header(vault: &Path) -> Vec<u8> {
    let mut w = W(MAGIC.to_vec());
    w.u32(FORMAT);
    w.str(env!("CARGO_PKG_VERSION"));
    w.str(&vault.to_string_lossy());
    w.0
}

struct W(Vec<u8>);

impl W {
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn strs(&mut self, v: &[String]) {
        self.u32(v.len() as u32);
        v.iter().for_each(|s| self.str(s));
    }
}

struct R<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn str(&mut self) -> Option<&'a str> {
        let n = self.u32()? as usize;
        std::str::from_utf8(self.take(n)?).ok()
    }
    fn strs(&mut self) -> Option<Vec<String>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| self.str().map(str::to_string)).collect()
    }
}

fn encode(w: &mut W, rel: &str, mtime_ns: u64, size: u64, n: &Note) {
    let start = w.0.len();
    w.u32(0); // record length, filled in below
    w.str(rel);
    w.u64(mtime_ns);
    w.u64(size);
    w.str(&n.content);
    w.u64(n.words as u64);
    w.strs(&n.tags);
    w.str(&Value::Object(n.props.clone()).to_string());
    w.strs(&n.wiki);
    w.strs(&n.mdl);
    let len = (w.0.len() - start - 4) as u32;
    w.0[start..start + 4].copy_from_slice(&len.to_le_bytes());
}

fn decode(rec: &[u8]) -> Option<(String, Entry)> {
    let mut r = R { b: rec, i: 0 };
    let rel = r.str()?.to_string();
    let (mtime_ns, size) = (r.u64()?, r.u64()?);
    let content = r.str()?.to_string();
    let words = r.u64()? as usize;
    let tags = r.strs()?;
    let props: Map<String, Value> = match serde_json::from_str(r.str()?).ok()? {
        Value::Object(m) => m,
        _ => return None,
    };
    let (wiki, mdl) = (r.strs()?, r.strs()?);
    let id = rel.strip_suffix(".md")?.to_string();
    let (folder, title) = match id.rsplit_once('/') {
        Some((f, t)) => (f.to_string(), t.to_string()),
        None => (String::new(), id.clone()),
    };
    let note = Note {
        id,
        title,
        folder,
        lc: content.to_lowercase(),
        content,
        mtime: mtime_ns / 1_000_000_000,
        rev: mtime_ns.to_string(),
        words,
        tags,
        props,
        wiki,
        mdl,
    };
    Some((rel, (mtime_ns, size, Arc::new(note))))
}

/// The cached notes of `vault`, or None when there is no usable cache.
pub fn load(file: &Path, vault: &Path) -> Option<Files> {
    let bytes = fs::read(file).ok()?;
    let head = header(vault);
    let body = bytes.strip_prefix(head.as_slice())?;
    let mut r = R { b: body, i: 0 };
    let count = r.u64()? as usize;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let n = r.u32()? as usize;
        records.push(r.take(n)?);
    }
    if r.i != body.len() {
        return None;
    }
    // decoding (and lowercasing every note for search) is the slow part: all cores
    map_parallel(&records, |rec| decode(rec)).into_iter().collect()
}

/// Write the cache atomically (temp file + rename). Saves are serialized so two never interleave.
pub fn save(file: &Path, vault: &Path, notes: &[(String, u64, u64, Arc<Note>)]) -> std::io::Result<()> {
    static SAVING: Mutex<()> = Mutex::new(());
    let _one_at_a_time = SAVING.lock().unwrap_or_else(|e| e.into_inner());
    let mut w = W(header(vault));
    w.u64(notes.len() as u64);
    for (rel, mtime_ns, size, n) in notes {
        encode(&mut w, rel, *mtime_ns, *size, n);
    }
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&w.0)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}
