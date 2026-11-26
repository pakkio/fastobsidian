//! Note operations: safe paths, read, save, create, rename (with link rewriting) and trash.
use crate::vault::{
    max_note_bytes, mdlink_re, normalize_newlines, resolve_md, strip_md_suffix, unquote, walk_vault, VaultIndex,
};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub enum Error {
    /// bad input (HTTP 400)
    Invalid(String),
    /// HTTP 404
    NotFound,
    /// the note changed on disk since the caller read it (HTTP 409)
    Conflict(String),
    /// anything else (HTTP 500)
    Io(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Invalid(s) | Error::Conflict(s) | Error::Io(s) => f.write_str(s),
            Error::NotFound => f.write_str("not found"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound
        } else {
            Error::Io(e.to_string())
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid<T>(s: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(s.into()))
}

fn too_large() -> Error {
    Error::Invalid(format!("note is larger than {} MiB", max_note_bytes() >> 20))
}

fn is_link(p: &Path) -> bool {
    // symlink_metadata does not follow links; on Windows junctions count as symlinks too
    fs::symlink_metadata(p).map(|m| m.file_type().is_symlink()).unwrap_or(false)
}

/// Fail if `p`, or any folder between the vault and `p`, is a symlink or junction.
/// Reads and writes never follow links, so a link inside the vault cannot redirect them to a file elsewhere.
pub fn no_links(vault: &Path, p: &Path) -> Result<PathBuf> {
    let rel = p.strip_prefix(vault).map_err(|_| Error::Invalid("path escapes the vault".into()))?;
    let mut cur = vault.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Normal(part) => cur.push(part),
            _ => return invalid("path escapes the vault"),
        }
        if is_link(&cur) {
            let shown = cur.strip_prefix(vault).unwrap_or(&cur).to_string_lossy().replace('\\', "/");
            return invalid(format!("refusing to follow a symlink: {shown}"));
        }
    }
    Ok(p.to_path_buf())
}

/// 'Folder/Name' (optionally with .md, surrounding spaces or slashes) -> 'Folder/Name'
pub fn norm_id(nid: &str) -> String {
    let n = nid.trim().trim_matches('/');
    strip_md_suffix(n).to_string()
}

fn bad_char(c: char) -> bool {
    matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (c as u32) < 0x20
}

/// Map a note id ('Folder/Name') to a .md path inside the vault, or fail.
pub fn note_path(vault: &Path, nid: &str) -> Result<PathBuf> {
    let nid = norm_id(nid);
    let bad = nid.is_empty()
        || nid
            .split('/')
            .any(|s| s.trim().is_empty() || s.starts_with('.') || s.ends_with('.') || s.chars().any(bad_char));
    if bad {
        return invalid(format!("invalid note name: '{nid}'"));
    }
    let mut p = vault.to_path_buf();
    for s in nid.split('/') {
        p.push(s);
    }
    let mut name = p.file_name().unwrap().to_os_string();
    name.push(".md");
    p.set_file_name(name);
    no_links(vault, &p)
}

fn check_size(len: u64) -> Result<()> {
    if len > max_note_bytes() {
        Err(too_large())
    } else {
        Ok(())
    }
}

fn mtime_of(p: &Path) -> Result<u64> {
    Ok(crate::vault::file_mtime_ns(&fs::metadata(p)?))
}

pub fn read_note(vault: &Path, nid: &str) -> Result<Value> {
    let p = note_path(vault, nid)?;
    if !p.is_file() {
        return Err(Error::NotFound);
    }
    check_size(fs::metadata(&p)?.len())?;
    let raw = normalize_newlines(&String::from_utf8_lossy(&fs::read(&p)?));
    Ok(json!({"raw": raw, "mtime": mtime_of(&p)?}))
}

/// The browser sends the mtime back as a JS number, which cannot hold every nanosecond value exactly:
/// compare at the precision a JS number has.
fn same_mtime(disk: u64, sent: &Value) -> bool {
    match sent {
        Value::Number(n) => n.as_u64() == Some(disk) || n.as_f64() == Some(disk as f64),
        _ => false,
    }
}

pub fn save_note(
    vault: &Path,
    idx: &VaultIndex,
    nid: &str,
    raw: &str,
    mtime: Option<&Value>,
    append: bool,
) -> Result<Value> {
    let p = note_path(vault, nid)?;
    if !p.is_file() {
        return Err(Error::NotFound);
    }
    if let Some(m) = mtime.filter(|m| !m.is_null() && m.as_f64() != Some(0.0)) {
        if !same_mtime(mtime_of(&p)?, m) {
            return Err(Error::Conflict("file changed on disk since you opened it".into()));
        }
    }
    check_size(raw.len() as u64)?;
    let data = if append {
        check_size(fs::metadata(&p)?.len() + raw.len() as u64)?;
        let old = String::from_utf8_lossy(&fs::read(&p)?).into_owned();
        let sep = if old.is_empty() || old.ends_with('\n') { "" } else { "\n" };
        format!("{old}{sep}{raw}")
    } else {
        raw.to_string()
    };
    fs::write(&p, data.as_bytes())?;
    idx.invalidate();
    Ok(json!({"ok": true, "mtime": mtime_of(&p)?}))
}

pub fn create_note(vault: &Path, idx: &VaultIndex, nid: &str, raw: &str) -> Result<Value> {
    let p = note_path(vault, nid)?;
    if p.exists() || is_link(&p) {
        return invalid("a note with that name already exists");
    }
    check_size(raw.len() as u64)?;
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(&p, raw.as_bytes())?;
    idx.invalidate();
    Ok(json!({"ok": true, "mtime": mtime_of(&p)?}))
}

pub fn rename_note(vault: &Path, idx: &VaultIndex, nid: &str, new: &str) -> Result<Value> {
    let (src, dst) = (note_path(vault, nid)?, note_path(vault, new)?);
    if !src.is_file() {
        return Err(Error::NotFound);
    }
    if (dst.exists() || is_link(&dst)) && dst != src {
        return invalid("target already exists");
    }
    let old_lookup = idx.build(true).lookup.clone();
    fs::create_dir_all(dst.parent().unwrap())?;
    fs::rename(&src, &dst)?;
    let n = rewrite_links(vault, &norm_id(nid), &norm_id(new), &old_lookup);
    idx.invalidate();
    Ok(json!({"ok": true, "updated": n}))
}

/// Move to <vault>/.trash (recoverable).
pub fn trash_note(vault: &Path, idx: &VaultIndex, nid: &str) -> Result<Value> {
    let p = note_path(vault, nid)?;
    if !p.is_file() {
        return Err(Error::NotFound);
    }
    let trash = no_links(vault, &vault.join(".trash"))?;
    fs::create_dir_all(&trash)?;
    let name = p.file_name().unwrap().to_string_lossy().into_owned();
    let mut dst = trash.join(&name);
    if dst.exists() {
        let stem = p.file_stem().unwrap().to_string_lossy();
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        dst = trash.join(format!("{stem}-{secs}.md"));
    }
    fs::rename(&p, &dst)?;
    idx.invalidate();
    Ok(json!({"ok": true, "trashed": dst.file_name().unwrap().to_string_lossy()}))
}

fn dirname(id: &str) -> &str {
    id.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// `os.path.relpath` for vault-relative, '/'-separated paths.
pub fn relpath(path: &str, start: &str) -> String {
    let p: Vec<&str> = path.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    let s: Vec<&str> = start.split('/').filter(|s| !s.is_empty() && *s != ".").collect();
    let common = p.iter().zip(&s).take_while(|(a, b)| a == b).count();
    let mut out: Vec<&str> = vec![".."; s.len() - common];
    out.extend(&p[common..]);
    if out.is_empty() {
        ".".into()
    } else {
        out.join("/")
    }
}

/// After a rename, repoint [[old]] / [[folder/old]] wikilinks and [text](old.md) links.
///
/// `lookup` must be the note lookup from *before* the rename (to resolve markdown links).
/// Only notes the index sees are touched: no dot-folders, no symlinks, nothing over MAX_NOTE_BYTES.
/// Line endings are kept as they are.
pub fn rewrite_links(vault: &Path, old_id: &str, new_id: &str, lookup: &Map<String, Value>) -> usize {
    let old_base = old_id.rsplit('/').next().unwrap_or(old_id);
    let new_base = new_id.rsplit('/').next().unwrap_or(new_id);
    let mut pairs = vec![(old_id, new_id)];
    if old_base != new_base {
        pairs.push((old_base, new_base));
    }
    let wiki: Vec<(fancy_regex::Regex, &str)> = pairs
        .iter()
        .map(|(old, rep)| {
            (fancy_regex::Regex::new(&format!(r"(?i)(\[\[){}(?=[\]|#])", regex::escape(old))).unwrap(), *rep)
        })
        .collect();
    let moved_dir = dirname(old_id) != dirname(new_id);
    let mut changed = 0;
    for f in walk_vault(vault).0 {
        let me_new = f.rel[..f.rel.len() - 3].to_string();
        // a renamed note keeps its own relative links pointing at the old location, so use old id for resolution
        let me_old = if me_new == new_id { old_id.to_string() } else { me_new.clone() };
        let Ok(bytes) = fs::read(&f.path) else { continue };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let mut new = text.clone();
        for (re, rep) in &wiki {
            new = re.replace_all(&new, |c: &fancy_regex::Captures<str>| format!("{}{}", &c[1], rep)).into_owned();
        }
        new = mdlink_re()
            .replace_all(&new, |m: &fancy_regex::Captures<str>| {
                let whole = m.get(0).unwrap().as_str();
                let raw = m.get(1).or(m.get(2)).unwrap().as_str();
                let Some(dest) = resolve_md(raw, &me_old, lookup) else { return whole.to_string() };
                if dest != old_id && !(me_new == new_id && moved_dir) {
                    return whole.to_string(); // unrelated link: leave its style alone
                }
                let dest = if dest == old_id { new_id.to_string() } else { dest };
                let mut target = relpath(&dest, dirname(&me_new)) + ".md";
                if m.get(2).is_some() && target.contains(' ') {
                    target = target.replace(' ', "%20");
                }
                if unquote(&target) != unquote(raw) {
                    whole.replacen(raw, &target, 1)
                } else {
                    whole.to_string()
                }
            })
            .into_owned();
        if new != text && fs::write(&f.path, new.as_bytes()).is_ok() {
            changed += 1;
        }
    }
    changed
}
