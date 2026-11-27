//! Today's daily note and pasted images, following the vault's Obsidian settings
//! (.obsidian/daily-notes.json and the attachment folder in .obsidian/app.json).
use crate::ops::{create_note, no_links, norm_id, note_path, Error, Result};
use crate::vault::{max_note_bytes, VaultIndex, MEDIA_EXT};
use chrono::{DateTime, Datelike, Local};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A JSON object from <vault>/.obsidian/<name>, or {} when missing or unreadable.
fn obsidian_json(vault: &Path, name: &str) -> Value {
    let p = vault.join(".obsidian").join(name);
    let ok = no_links(vault, &p).is_ok() && fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() <= 1 << 20);
    ok.then(|| fs::read(&p).ok())
        .flatten()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Format `t` with a moment.js pattern, the syntax Obsidian uses (YYYY-MM-DD, dddd, [literal], …).
pub fn moment_format(t: &DateTime<Local>, pattern: &str) -> String {
    const TOKENS: &[(&str, &str)] = &[
        ("YYYY", "%Y"),
        ("YY", "%y"),
        ("MMMM", "%B"),
        ("MMM", "%b"),
        ("MM", "%m"),
        ("M", "%-m"),
        ("Do", ""),
        ("DD", "%d"),
        ("D", "%-d"),
        ("dddd", "%A"),
        ("ddd", "%a"),
        ("HH", "%H"),
        ("H", "%-H"),
        ("mm", "%M"),
        ("ss", "%S"),
    ];
    let mut out = String::new();
    let mut rest = pattern;
    'outer: while let Some(c) = rest.chars().next() {
        if c == '[' {
            if let Some(end) = rest.find(']') {
                out += &rest[1..end];
                rest = &rest[end + 1..];
                continue;
            }
        }
        for (tok, fmt) in TOKENS {
            if let Some(r) = rest.strip_prefix(tok) {
                if *tok == "Do" {
                    let d = t.day();
                    let suffix = match (d % 10, d % 100) {
                        (_, 11..=13) => "th",
                        (1, _) => "st",
                        (2, _) => "nd",
                        (3, _) => "rd",
                        _ => "th",
                    };
                    out += &format!("{d}{suffix}");
                } else {
                    out += &t.format(fmt).to_string();
                }
                rest = r;
                continue 'outer;
            }
        }
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// Fill Obsidian's core template variables: {{title}}, {{date}}, {{time}}, {{date:FORMAT}}, {{time:FORMAT}}.
fn fill_template(text: &str, title: &str, t: &DateTime<Local>) -> String {
    let re = regex::Regex::new(r"\{\{\s*(title|date|time)\s*(?::([^}]*))?\}\}").unwrap();
    re.replace_all(text, |c: &regex::Captures| match &c[1] {
        "title" => title.to_string(),
        kind => {
            let default = if kind == "date" { "YYYY-MM-DD" } else { "HH:mm" };
            moment_format(t, c.get(2).map_or(default, |m| m.as_str().trim()))
        }
    })
    .into_owned()
}

/// Today's daily note: its id, created first (from the configured template) if it does not exist yet.
pub fn daily_note(vault: &Path, idx: &VaultIndex) -> Result<Value> {
    let cfg = obsidian_json(vault, "daily-notes.json");
    let now = Local::now();
    let text = |k: &str| cfg[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    let name = moment_format(&now, text("format").unwrap_or("YYYY-MM-DD"));
    let folder = text("folder").unwrap_or("").trim_matches('/');
    let id = norm_id(&if folder.is_empty() { name.clone() } else { format!("{folder}/{name}") });
    if note_path(vault, &id)?.is_file() {
        return Ok(json!({"ok": true, "id": id, "created": false}));
    }
    let template = match text("template") {
        Some(t) => {
            let p = note_path(vault, t)?;
            no_links(vault, &p)?;
            match fs::read(&p) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(_) => String::new(), // a missing template gives an empty note, as in Obsidian
            }
        }
        None => String::new(),
    };
    let title = id.rsplit('/').next().unwrap_or(&id).to_string();
    create_note(vault, idx, &id, &fill_template(&template, &title, &now))?;
    Ok(json!({"ok": true, "id": id, "created": true}))
}

fn bad_segment(s: &str) -> bool {
    let s = s.trim();
    s.is_empty()
        || s.starts_with('.')
        || s.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (c as u32) < 0x20)
}

/// The folder (relative to the vault) where attachments of `note` go, from Obsidian's "attachmentFolderPath".
fn attachment_folder(vault: &Path, note: &str) -> Result<String> {
    let setting = obsidian_json(vault, "app.json")["attachmentFolderPath"].as_str().unwrap_or("").trim().to_string();
    let note = norm_id(note);
    let note_dir = note.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let rel = if setting.is_empty() || setting == "/" {
        String::new()
    } else if setting == "." || setting == "./" {
        note_dir.to_string()
    } else if let Some(sub) = setting.strip_prefix("./") {
        [note_dir, sub.trim_matches('/')].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join("/")
    } else {
        setting.trim_matches('/').to_string()
    };
    if !rel.is_empty() && rel.split('/').any(bad_segment) {
        return Err(Error::Invalid(format!("bad attachment folder: '{rel}'")));
    }
    Ok(rel)
}

/// Save an image (or other media file) next to `note` as Obsidian would, never overwriting a file.
/// Returns its vault path and the text to embed: `![[<embed>]]`.
pub fn save_attachment(vault: &Path, idx: &VaultIndex, note: &str, name: &str, bytes: &[u8]) -> Result<Value> {
    if bytes.len() as u64 > max_note_bytes() {
        return Err(Error::Invalid(format!("file is larger than {} MiB", max_note_bytes() >> 20)));
    }
    let base = name.rsplit(['/', '\\']).next().unwrap_or("").trim();
    let clean: String = base
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '[' | ']' | '#' | '^') || (c as u32) < 0x20 {
                '-'
            } else {
                c
            }
        })
        .collect();
    let (stem, ext) = clean.rsplit_once('.').ok_or(Error::Invalid("the file name needs an extension".into()))?;
    let ext = ext.to_lowercase();
    if !MEDIA_EXT.contains(&ext.as_str()) {
        return Err(Error::Invalid(format!("not an image or media file: .{ext}")));
    }
    let stem = stem.trim().trim_start_matches('.');
    let stem = if stem.is_empty() { "image" } else { stem };

    let folder = attachment_folder(vault, note)?;
    let mut dir: PathBuf = vault.to_path_buf();
    folder.split('/').filter(|s| !s.is_empty()).for_each(|s| dir.push(s));
    no_links(vault, &dir)?;
    fs::create_dir_all(&dir)?;
    no_links(vault, &dir)?;

    let known = idx.build(false); // attachments before this one
    for n in 0..1000 {
        let file = if n == 0 { format!("{stem}.{ext}") } else { format!("{stem} {n}.{ext}") };
        let path = dir.join(&file);
        let created = fs::OpenOptions::new().write(true).create_new(true).open(&path);
        let mut f = match created {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        };
        if let Err(e) = f.write_all(bytes) {
            drop(f);
            let _ = fs::remove_file(&path);
            return Err(e.into());
        }
        let rel = if folder.is_empty() { file.clone() } else { format!("{folder}/{file}") };
        // the bare name, like Obsidian, unless another attachment in the vault already has it
        let taken = known.assets.contains_key(&file.to_lowercase());
        idx.invalidate();
        let embed = if taken { rel.clone() } else { file };
        return Ok(json!({"ok": true, "path": rel, "embed": embed}));
    }
    Err(Error::Invalid("too many files with that name".into()))
}
