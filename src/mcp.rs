//! Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0).
use crate::ops::{create_note, norm_id, note_path, read_note, rename_note, save_note, trash_note, Error};
use crate::vault::VaultIndex;
use serde_json::{json, Map, Value};
use std::io::{BufRead, Write};
use std::path::Path;

enum ToolError {
    Missing(&'static str),
    Op(Error),
}

impl From<Error> for ToolError {
    fn from(e: Error) -> Self {
        ToolError::Op(e)
    }
}

type ToolResult = Result<Value, ToolError>;

fn arg<'a>(a: &'a Value, k: &'static str) -> Result<&'a str, ToolError> {
    a.get(k).and_then(Value::as_str).ok_or(ToolError::Missing(k))
}

fn str_list(a: &Value, k: &str) -> Vec<String> {
    a.get(k)
        .and_then(Value::as_array)
        .map(|v| v.iter().filter_map(Value::as_str).map(String::from).collect())
        .unwrap_or_default()
}

fn frontmatter(tags: &[String], aliases: &[String]) -> String {
    let mut lines = vec!["---".to_string()];
    for (key, vals) in [("tags", tags), ("aliases", aliases)] {
        if !vals.is_empty() {
            lines.push(format!("{key}:"));
            lines.extend(vals.iter().map(|v| format!("  - {v}")));
        }
    }
    lines.push("---".into());
    lines.push(String::new());
    lines.join("\n")
}

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub properties: Value,
    pub required: &'static [&'static str],
}

pub fn tools() -> Vec<Tool> {
    let s = json!({"type": "string"});
    let a = json!({"type": "array", "items": {"type": "string"}});
    vec![
        Tool { name: "search", description: "Ranked search (name > alias > tag > body). Empty query with tag lists that tag's notes.",
               properties: json!({"query": s, "tag": s, "limit": {"type": "integer"}}), required: &[] },
        Tool { name: "read", description: "Read a note's raw markdown by id, e.g. 'Folder/Name'.",
               properties: json!({"id": s}), required: &["id"] },
        Tool { name: "write", description: "Create a note, or append to / overwrite an existing one. mode: append (default), overwrite, create. tags/aliases become frontmatter on new notes.",
               properties: json!({"id": s, "content": s, "mode": s, "tags": a, "aliases": a}), required: &["id", "content"] },
        Tool { name: "index", description: "All note ids with tags, tag counts, orphan notes and unresolved links.",
               properties: json!({}), required: &[] },
        Tool { name: "backlinks", description: "Ids of notes that link to the given note.",
               properties: json!({"id": s}), required: &["id"] },
        Tool { name: "rename", description: "Rename/move a note and repoint wikilinks and markdown links.",
               properties: json!({"id": s, "new": s}), required: &["id", "new"] },
        Tool { name: "delete", description: "Delete a note (moved to .trash, recoverable).",
               properties: json!({"id": s}), required: &["id"] },
    ]
}

fn call(vault: &Path, idx: &VaultIndex, name: &str, a: &Value) -> ToolResult {
    match name {
        "search" => {
            let limit = match a.get("limit") {
                Some(Value::Number(n)) => n.as_u64().unwrap_or(20) as usize,
                Some(Value::String(s)) => s.trim().parse().map_err(|_| Error::Invalid(format!("bad limit: {s}")))?,
                _ => 20,
            };
            let q = a.get("query").and_then(Value::as_str).unwrap_or("");
            let tag = a.get("tag").and_then(Value::as_str).unwrap_or("");
            Ok(Value::Array(idx.search(q, limit, tag)))
        }
        "read" => Ok(read_note(vault, arg(a, "id")?)?["raw"].clone()),
        "write" => {
            let nid = arg(a, "id")?;
            let mut content = a.get("content").and_then(Value::as_str).unwrap_or("").to_string();
            let mode = a.get("mode").and_then(Value::as_str).unwrap_or("append");
            if !matches!(mode, "append" | "overwrite" | "create") {
                return Err(Error::Invalid("mode must be append, overwrite or create".into()).into());
            }
            let exists = note_path(vault, nid)?.is_file();
            if exists && mode == "create" {
                return Err(Error::Invalid("a note with that name already exists".into()).into());
            }
            if exists {
                return Ok(save_note(vault, idx, nid, &content, None, mode == "append")?);
            }
            let (tags, aliases) = (str_list(a, "tags"), str_list(a, "aliases"));
            if (!tags.is_empty() || !aliases.is_empty()) && !content.starts_with("---") {
                content = frontmatter(&tags, &aliases) + &content;
            }
            Ok(create_note(vault, idx, nid, &content)?)
        }
        "index" => {
            let res = idx.build(false);
            let mut tags: Vec<(String, usize)> = vec![];
            let mut linked = std::collections::HashSet::new();
            for n in &res.notes {
                for t in n["tags"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    match tags.iter_mut().find(|(k, _)| k == t) {
                        Some(e) => e.1 += 1,
                        None => tags.push((t.to_string(), 1)),
                    }
                }
                let links = n["links"].as_array().cloned().unwrap_or_default();
                if !links.is_empty() {
                    linked.insert(n["id"].as_str().unwrap_or("").to_string());
                    linked.extend(links.iter().filter_map(Value::as_str).map(String::from));
                }
            }
            tags.sort_by_key(|t| std::cmp::Reverse(t.1)); // stable: ties keep first-seen order
            let tag_map: Map<String, Value> = tags.into_iter().map(|(k, v)| (k, json!(v))).collect();
            Ok(json!({
                "notes": res.notes.iter().map(|n| json!({"id": n["id"], "tags": n["tags"], "mtime": n["mtime"]})).collect::<Vec<_>>(),
                "tags": tag_map,
                "orphans": res.note_ids().filter(|id| !linked.contains(*id)).collect::<Vec<_>>(),
                "unresolved": res.unresolved.iter().map(|(t, s)| json!({"target": t, "sources": s})).collect::<Vec<_>>(),
            }))
        }
        "backlinks" => {
            let nid = norm_id(arg(a, "id")?);
            let res = idx.build(false);
            Ok(json!(res
                .notes
                .iter()
                .filter(|n| n["links"].as_array().is_some_and(|l| l.iter().any(|x| x.as_str() == Some(nid.as_str()))))
                .map(|n| n["id"].clone())
                .collect::<Vec<_>>()))
        }
        "rename" => Ok(rename_note(vault, idx, arg(a, "id")?, arg(a, "new")?)?),
        "delete" => Ok(trash_note(vault, idx, arg(a, "id")?)?),
        _ => Err(Error::Invalid(format!("unknown tool: {name}")).into()),
    }
}

/// Handle one JSON-RPC message; None for notifications (no reply).
pub fn handle(vault: &Path, idx: &VaultIndex, line: &str) -> Option<Value> {
    let reply = |id: Value, result: Result<Value, Value>| {
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        })
    };
    let req: Value = match serde_json::from_str(line) {
        Ok(v @ Value::Object(_)) => v,
        _ => return reply(Value::Null, Err(json!({"code": -32700, "message": "parse error"}))),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    if id.is_null() {
        return None; // notification (e.g. notifications/initialized)
    }
    let method = req["method"].as_str().unwrap_or("");
    let params = req.get("params").filter(|p| p.is_object()).cloned().unwrap_or_else(|| json!({}));
    match method {
        "initialize" => reply(
            id,
            Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-03-26"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fastobsidian", "version": env!("CARGO_PKG_VERSION")},
            })),
        ),
        "ping" => reply(id, Ok(json!({}))),
        "tools/list" => reply(
            id,
            Ok(json!({"tools": tools().iter().map(|t| json!({
            "name": t.name, "description": t.description,
            "inputSchema": {"type": "object", "properties": t.properties, "required": t.required},
        })).collect::<Vec<_>>()})),
        ),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            if !tools().iter().any(|t| t.name == name) {
                return reply(id, Err(json!({"code": -32602, "message": format!("unknown tool: {name}")})));
            }
            let args = params.get("arguments").filter(|a| a.is_object()).cloned().unwrap_or_else(|| json!({}));
            let (text, bad) = match call(vault, idx, name, &args) {
                Ok(Value::String(s)) => (s, false),
                Ok(v) => (v.to_string(), false),
                Err(ToolError::Missing(k)) => (format!("missing argument: '{k}'"), true),
                Err(ToolError::Op(Error::NotFound)) => ("note not found".into(), true),
                Err(ToolError::Op(e)) => (e.to_string(), true),
            };
            reply(id, Ok(json!({"content": [{"type": "text", "text": text}], "isError": bad})))
        }
        _ => reply(id, Err(json!({"code": -32601, "message": format!("method not found: {method}")}))),
    }
}

pub fn run(vault: &Path) {
    let idx = VaultIndex::new(vault);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(msg) = handle(vault, &idx, &line) {
            let _ = writeln!(out, "{msg}");
            let _ = out.flush();
        }
    }
}
