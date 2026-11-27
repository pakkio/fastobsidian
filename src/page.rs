//! The HTML page (one template, filled in a single pass) and the vault's Obsidian theme.
use crate::ops::no_links;
use crate::vault::{build_with_content, max_note_bytes, VaultIndex};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

pub const TEMPLATE: &str = include_str!("../explorer_template.html");

pub struct ObsidianCss {
    pub css: String,
    /// "dark" / "light" from Obsidian's base theme, if set
    pub mode: Option<&'static str>,
    pub theme: String,
    /// enabled CSS snippets that were found
    pub snippets: Vec<String>,
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.chars().any(|c| c == '/' || c == '\\' || (c as u32) < 0x20)
}

/// The theme and CSS snippets enabled in <vault>/.obsidian/appearance.json. Missing files are skipped.
pub fn obsidian_css(vault: &Path) -> ObsidianCss {
    let base = vault.join(".obsidian");
    let read = |p: &Path| -> Option<String> {
        no_links(vault, p).ok()?;
        let m = fs::metadata(p).ok()?;
        if !m.is_file() || m.len() > max_note_bytes() {
            return None;
        }
        Some(String::from_utf8_lossy(&fs::read(p).ok()?).into_owned())
    };
    let app: Value = read(&base.join("appearance.json"))
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let (mut parts, mut snippets) = (vec![], vec![]);
    let theme = app["cssTheme"].as_str().unwrap_or("").to_string();
    if safe_name(&theme) {
        // current layout, then the pre-1.0 one
        for p in [base.join("themes").join(&theme).join("theme.css"), base.join("themes").join(format!("{theme}.css"))]
        {
            if let Some(css) = read(&p) {
                parts.push(format!("/* theme: {theme} */\n{css}"));
                break;
            }
        }
    }
    if let Some(list) = app["enabledCssSnippets"].as_array() {
        for s in list.iter().filter_map(Value::as_str).filter(|s| safe_name(s)) {
            if let Some(css) = read(&base.join("snippets").join(format!("{s}.css"))) {
                parts.push(format!("/* snippet: {s} */\n{css}"));
                snippets.push(s.to_string());
            }
        }
    }
    let mode = match app["theme"].as_str() {
        Some("obsidian") => Some("dark"),
        Some("moonstone") => Some("light"),
        _ => None,
    };
    ObsidianCss { css: parts.join("\n"), mode, theme, snippets }
}

/// Community plugins enabled in <vault>/.obsidian/community-plugins.json (listed, never run).
pub fn enabled_plugins(vault: &Path) -> Vec<String> {
    let p = vault.join(".obsidian").join("community-plugins.json");
    if no_links(vault, &p).is_err() || fs::metadata(&p).map_or(true, |m| !m.is_file() || m.len() > max_note_bytes()) {
        return vec![];
    }
    let list: Value = fs::read(&p).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    list.as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

fn placeholder() -> &'static regex::Regex {
    static R: OnceLock<regex::Regex> = OnceLock::new();
    R.get_or_init(|| regex::Regex::new(r"/\*__(\w+)__\*/(?:false|null)?").unwrap())
}

/// Fill the template in one pass, so text inserted from the vault is never scanned for placeholders.
/// `data` is the vault JSON; `native` is true inside the app window.
pub fn render(vault: &Path, live: bool, data: &str, theme: bool, native: bool) -> String {
    let obs = theme.then(|| obsidian_css(vault));
    let style_end = regex::Regex::new(r"(?i)</(style)").unwrap();
    placeholder()
        .replace_all(TEMPLATE, |c: &regex::Captures| match &c[1] {
            "LIVE" => live.to_string(),
            "NATIVE" => native.to_string(),
            "DATA" => data.replace("</", "<\\/"),
            "OBS" => match &obs {
                Some(o) => json!({"mode": o.mode, "theme": o.theme}).to_string().replace("</", "<\\/"),
                None => "null".into(),
            },
            "OBSCSS" => obs.as_ref().map(|o| style_end.replace_all(&o.css, "<\\/$1").into_owned()).unwrap_or_default(),
            _ => c[0].to_string(),
        })
        .into_owned()
}

/// Page for the live server / app window.
pub fn live_page(idx: &VaultIndex, theme: bool, native: bool) -> String {
    render(&idx.vault, true, &idx.json(), theme, native)
}

/// Self-contained read-only page with all note content embedded.
pub fn static_page(vault: &Path, theme: bool) -> String {
    render(vault, false, &build_with_content(vault).to_string(), theme, false)
}
