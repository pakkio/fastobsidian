//! Index, parsing, note operations, the local server and the MCP server.
use fastobsidian::ops::{self, note_path, read_note, rename_note, save_note, trash_note, Error};
use fastobsidian::server::{Options, Server};
use fastobsidian::vault::VaultIndex;
use fastobsidian::{mcp, page};
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn make_vault(files: &[(&str, &str)]) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("fo-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    for (name, text) in files {
        let p = d.join(name);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    std::path::absolute(d).unwrap()
}

fn read(p: PathBuf) -> String {
    fs::read_to_string(p).unwrap()
}

fn ids(idx: &VaultIndex) -> Vec<String> {
    idx.build(true).note_ids().map(String::from).collect()
}

fn core_vault() -> (PathBuf, VaultIndex) {
    let v = make_vault(&[
        ("Docker.md", "---\naliases: [Containers]\ntags: [infra]\n---\nbody about whales"),
        ("a.md", "see [[Containers]] and [[Docker]] and [[nope]]"),
        ("Other/Containers.md", "real note named containers"),
        ("b.md", "mentions nginx here"),
    ]);
    let idx = VaultIndex::new(&v);
    (v, idx)
}

#[test]
fn alias_resolves_but_real_name_wins() {
    let (_, idx) = core_vault();
    let res = idx.build(false);
    assert_eq!(res.lookup["containers"], "Other/Containers");
    assert_eq!(res.unresolved.iter().map(|u| u.0.as_str()).collect::<Vec<_>>(), ["nope"]);
}

#[test]
fn alias_link_when_no_clash() {
    let (v, idx) = core_vault();
    fs::remove_file(v.join("Other/Containers.md")).unwrap();
    idx.invalidate();
    let res = idx.build(false);
    assert_eq!(res.lookup["containers"], "Docker");
    let a = res.notes.iter().find(|n| n["id"] == "a").unwrap();
    assert!(a["links"].as_array().unwrap().contains(&json!("Docker")));
}

#[test]
fn search_ranking_and_tag() {
    let (_, idx) = core_vault();
    let s = |q: &str, tag: &str| {
        idx.search(q, 300, tag).iter().map(|r| r["id"].as_str().unwrap().to_string()).collect::<Vec<_>>()
    };
    assert_eq!(s("docker", "")[0], "Docker");
    assert_eq!(s("containers", "")[..2], ["Other/Containers", "Docker"]);
    assert_eq!(s("", "infra"), ["Docker"]);
    assert_eq!(s("nginx", "")[0], "b");
    assert!(idx.search("nginx", 300, "")[0]["snippet"].as_str().unwrap().contains("nginx"));
}

#[test]
fn frontmatter_and_tags() {
    let v = make_vault(&[(
        "n.md",
        "---\ntags:\n  - one\n  - \"two\"\nstatus: draft\naliases: x, y\n---\n# T\nbody #three `#notcode` and ```\n#nofence\n``` [[Wiki#Head|shown]]",
    )]);
    let idx = VaultIndex::new(&v);
    let res = idx.build(false);
    let n = &res.notes[0];
    assert_eq!(n["tags"], json!(["one", "three", "two"]));
    assert_eq!(n["props"], json!({"status": "draft", "aliases": ["x", "y"]}));
    assert_eq!(res.lookup["x"], "n");
    assert_eq!(res.unresolved[0].0, "Wiki");
}

#[test]
fn append_and_conflict() {
    let (v, idx) = core_vault();
    save_note(&v, &idx, "b", "more", None, true).unwrap();
    assert!(read(v.join("b.md")).ends_with("here\nmore"));
    assert!(matches!(save_note(&v, &idx, "b", "x", Some(&json!(1)), false), Err(Error::Conflict(_))));
    // the mtime the page got back from read_note is accepted
    let m = read_note(&v, "b").unwrap()["mtime"].clone();
    let as_js = json!(m.as_f64().unwrap()); // what a browser sends back: a double
    save_note(&v, &idx, "b", "new", Some(&as_js), false).unwrap();
}

#[test]
fn rename_rewrites_wikilinks() {
    let (v, idx) = core_vault();
    rename_note(&v, &idx, "Docker", "Whale").unwrap();
    assert!(read(v.join("a.md")).contains("[[Whale]]"));
}

#[test]
fn traversal_rejected() {
    let (v, _) = core_vault();
    for bad in ["../x", "a/../../x", ".hidden/x", "C:/x", "a\\b", ""] {
        assert!(matches!(note_path(&v, bad), Err(Error::Invalid(_))), "{bad}");
    }
}

#[test]
fn trash() {
    let (v, idx) = core_vault();
    trash_note(&v, &idx, "b").unwrap();
    assert!(v.join(".trash/b.md").exists());
}

#[test]
fn markdown_links_rewritten_relative_to_each_note() {
    let v = make_vault(&[
        ("A/target.md", "x"),
        ("A/sib.md", "[t](target.md) and [h](target.md#Head)"),
        ("B/far.md", "[t](../A/target.md) [u](<../A/target.md>) [ext](https://e.com/target.md)"),
        ("root.md", "[t](/A/target.md) [[target]]\r\nline two\r\n"),
    ]);
    rename_note(&v, &VaultIndex::new(&v), "A/target", "C/moved").unwrap();
    assert_eq!(read(v.join("A/sib.md")), "[t](../C/moved.md) and [h](../C/moved.md#Head)");
    assert_eq!(read(v.join("B/far.md")), "[t](../C/moved.md) [u](<../C/moved.md>) [ext](https://e.com/target.md)");
    // root note: both links updated, its CRLF line endings kept
    assert_eq!(read(v.join("root.md")), "[t](C/moved.md) [[moved]]\r\nline two\r\n");
}

#[test]
fn moved_note_keeps_its_own_relative_links_working() {
    let v = make_vault(&[("A/n.md", "[s](sib.md)"), ("A/sib.md", "s")]);
    rename_note(&v, &VaultIndex::new(&v), "A/n", "B/n").unwrap();
    assert_eq!(read(v.join("B/n.md")), "[s](../A/sib.md)");
}

#[test]
fn relpath_like_posix() {
    assert_eq!(ops::relpath("C/moved", "A"), "../C/moved");
    assert_eq!(ops::relpath("A/x", "A"), "x");
    assert_eq!(ops::relpath("x", ""), "x");
    assert_eq!(ops::relpath("a/b", "a/b/c"), "..");
}

/// A symlink (Unix, or Windows with developer mode) or, for folders on Windows, a junction (needs no rights).
fn try_link(link: &Path, target: &Path, dir: bool) -> bool {
    #[cfg(unix)]
    {
        let _ = dir;
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        let ok = if dir {
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        } else {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        };
        ok || (dir
            && std::process::Command::new("cmd")
                .args(["/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .is_ok_and(|o| o.status.success()))
    }
}

#[test]
fn symlinked_note_is_ignored_and_refused() {
    let outside = make_vault(&[("secret.md", "[[x]] outside")]);
    let v = make_vault(&[("x.md", "x")]);
    if !try_link(&v.join("link.md"), &outside.join("secret.md"), false) {
        eprintln!("skipped: cannot create file symlinks here");
        return;
    }
    assert_eq!(ids(&VaultIndex::new(&v)), ["x"]);
    assert!(matches!(read_note(&v, "link"), Err(Error::Invalid(_))));
    rename_note(&v, &VaultIndex::new(&v), "x", "y").unwrap(); // must not rewrite the file behind the link
    assert_eq!(read(outside.join("secret.md")), "[[x]] outside");
}

#[test]
fn symlinked_folder_is_ignored_and_refused() {
    let outside = make_vault(&[("in.md", "hi")]);
    let v = make_vault(&[("x.md", "x")]);
    if !try_link(&v.join("dir"), &outside, true) {
        eprintln!("skipped: cannot create folder links here");
        return;
    }
    assert_eq!(ids(&VaultIndex::new(&v)), ["x"]);
    assert!(matches!(ops::create_note(&v, &VaultIndex::new(&v), "dir/new", "boo"), Err(Error::Invalid(_))));
    assert!(!outside.join("new.md").exists());
}

#[test]
fn version_changes_only_when_vault_changes() {
    let v = make_vault(&[("a.md", "one")]);
    let idx = VaultIndex::new(&v);
    idx.build(false);
    let v0 = idx.version();
    idx.build(true);
    assert_eq!(idx.version(), v0);
    fs::write(v.join("b.md"), "two").unwrap();
    idx.build(true);
    assert_eq!(idx.version(), v0 + 1);
    assert!(idx.build(false).notes[0]["rev"].is_string());
}

fn http(port: u16, req: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let code = out[9..12].parse().unwrap();
    (code, out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default())
}

#[test]
fn server_routes_and_guards() {
    let v = make_vault(&[("a.md", "one [[b]]"), ("img.png", "png"), ("b.md", "two")]);
    let srv =
        Server::bind(&v, Options { port: 0, theme: false, watch: 0.0, native: false, picker: None, cache: false })
            .unwrap();
    let port = srv.port;
    srv.spawn();
    let host = format!("Host: 127.0.0.1:{port}\r\n");
    let (c, body) = http(port, &format!("GET /api/data HTTP/1.1\r\n{host}\r\n"));
    assert_eq!(c, 200);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["notes"].as_array().unwrap().len(), 2);
    assert_eq!(http(port, "GET /api/data HTTP/1.1\r\nHost: evil.example\r\n\r\n").0, 403);
    let (c, body) = http(port, &format!("GET /api/search?q=two HTTP/1.1\r\n{host}\r\n"));
    assert_eq!((c, serde_json::from_str::<Value>(&body).unwrap()[0]["id"].clone()), (200, json!("b")));
    assert_eq!(http(port, &format!("GET /vault/img.png HTTP/1.1\r\n{host}\r\n")), (200, "png".into()));
    assert_eq!(http(port, &format!("GET /vault/a.md HTTP/1.1\r\n{host}\r\n")).0, 404);
    let put = |extra: &str| {
        let body = r#"{"id":"a","raw":"changed"}"#;
        http(port, &format!("PUT /api/note HTTP/1.1\r\n{host}{extra}Content-Length: {}\r\n\r\n{body}", body.len()))
    };
    assert_eq!(put("").0, 403); // no X-Explorer header
    assert_eq!(put("X-Explorer: 1\r\nOrigin: http://evil.example\r\n").0, 403);
    assert_eq!(put("X-Explorer: 1\r\n").0, 200);
    assert_eq!(read(v.join("a.md")), "changed");
    let (c, html) = http(port, &format!("GET / HTTP/1.1\r\n{host}\r\n"));
    assert_eq!(c, 200);
    assert!(html.contains("window.LIVE=true;") && html.contains("window.NATIVE=false;"));
}

#[test]
fn server_pushes_change_event() {
    let v = make_vault(&[("a.md", "one")]);
    let srv =
        Server::bind(&v, Options { port: 0, theme: false, watch: 0.1, native: false, picker: None, cache: false })
            .unwrap();
    let port = srv.port;
    srv.spawn();
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    s.write_all(format!("GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes()).unwrap();
    let mut r = BufReader::new(s);
    let mut lines = vec![];
    let mut line = String::new();
    while !lines.iter().any(|l: &String| l == "event: hello\n") {
        line.clear();
        r.read_line(&mut line).unwrap();
        lines.push(line.clone());
    }
    std::thread::sleep(Duration::from_millis(300));
    fs::write(v.join("b.md"), "two").unwrap();
    while !lines.iter().any(|l| l == "event: change\n") {
        line.clear();
        r.read_line(&mut line).unwrap();
        lines.push(line.clone());
        assert!(lines.len() < 60);
    }
}

#[test]
fn theme_and_snippets_are_embedded() {
    let v = make_vault(&[
        ("n.md", "a note that mentions /*__OBSCSS__*/ and </style>"),
        (
            ".obsidian/appearance.json",
            r#"{"cssTheme": "Neat", "theme": "moonstone", "enabledCssSnippets": ["wide", "missing", "../evil"]}"#,
        ),
        (".obsidian/themes/Neat/theme.css", ".theme-light{--background-primary:#abcdef}</style><script>"),
        (".obsidian/snippets/wide.css", "body{--file-line-width:900px}"),
        (".obsidian/evil.css", "nope"),
    ]);
    let obs = page::obsidian_css(&v);
    assert_eq!(obs.mode, Some("light"));
    assert!(obs.css.contains("#abcdef") && obs.css.contains("--file-line-width") && !obs.css.contains("nope"));
    let html = page::static_page(&v, true);
    assert!(html.contains("#abcdef"));
    assert!(!html.contains("</style><script>")); // theme CSS cannot close the style element
    assert!(html.contains(r#"window.OBS={"mode":"light","theme":"Neat"}"#));
    assert!(html.contains("mentions /*__OBSCSS__*/")); // vault text is not treated as a placeholder
    assert_eq!(html.matches("--file-line-width").count(), 1);
}

#[test]
fn no_theme_by_default() {
    let v = make_vault(&[
        ("n.md", "x"),
        (".obsidian/snippets/s.css", "body{}"),
        (".obsidian/appearance.json", r#"{"enabledCssSnippets":["s"]}"#),
    ]);
    let html = page::static_page(&v, false);
    assert!(html.contains("window.OBS=null"));
    assert!(html.contains(r#"<style id="obscss"></style>"#));
}

#[test]
fn mcp_messages() {
    let v = make_vault(&[("n.md", "hello world")]);
    let idx = VaultIndex::new(&v);
    let call = |line: Value| mcp::handle(&v, &idx, &line.to_string());
    assert!(call(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).is_none());
    assert_eq!(
        call(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}})).unwrap()["result"]["serverInfo"]
            ["name"],
        "fastobsidian"
    );
    assert_eq!(
        call(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        7
    );
    let tool = |name: &str, args: Value| {
        call(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": name, "arguments": args}}))
            .unwrap()["result"]
            .clone()
    };
    assert_eq!(tool("write", json!({"id": "Dir/new", "content": "hi", "tags": ["t"]}))["isError"], false);
    assert!(read(v.join("Dir/new.md")).starts_with("---\ntags:\n  - t\n---\nhi"));
    assert!(tool("search", json!({"query": "hello"}))["content"][0]["text"].as_str().unwrap().contains("\"n\""));
    assert_eq!(tool("read", json!({"id": "../etc"}))["isError"], true);
    assert_eq!(tool("read", json!({}))["content"][0]["text"], "missing argument: 'id'");
    assert_eq!(tool("read", json!({"id": "nope"}))["content"][0]["text"], "note not found");
    assert_eq!(
        call(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "x"}})).unwrap()["error"]
            ["code"],
        -32602
    );
    assert_eq!(call(json!({"jsonrpc": "2.0", "id": 5, "method": "nope"})).unwrap()["error"]["code"], -32601);
}

#[test]
fn mcp_over_stdio_binary() {
    let v = make_vault(&[("n.md", "hello world")]);
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fastobsidian"))
        .arg(&v)
        .arg("--mcp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "search", "arguments": {"query": "hello"}}}),
    ];
    let input: String = msgs.iter().map(|m| m.to_string() + "\n").collect();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let replies: Vec<Value> =
        String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1]["result"]["isError"], false);
}

#[test]
fn static_build_of_sample_vault() {
    let html = page::static_page(&std::path::absolute("sample-vault").unwrap(), false);
    assert!(html.contains("window.LIVE=false;") && html.contains("\"Home\""));
}

#[test]
fn moment_formats() {
    use chrono::TimeZone;
    let t = chrono::Local.with_ymd_and_hms(2026, 3, 2, 9, 5, 7).unwrap();
    let f = |p| fastobsidian::daily::moment_format(&t, p);
    assert_eq!(f("YYYY-MM-DD"), "2026-03-02");
    assert_eq!(f("dddd, MMMM Do YYYY"), "Monday, March 2nd 2026");
    assert_eq!(f("YYYY/MM/[Week] D"), "2026/03/Week 2");
    assert_eq!(f("HH:mm:ss"), "09:05:07");
}

#[test]
fn daily_note_uses_settings_and_template() {
    let v = make_vault(&[
        (".obsidian/daily-notes.json", r#"{"folder": "Journal", "format": "YYYY-MM-DD", "template": "Tpl/Day"}"#),
        ("Tpl/Day.md", "# {{title}}\ncreated {{date:YYYY}}\n"),
    ]);
    let idx = VaultIndex::new(&v);
    let r = fastobsidian::daily::daily_note(&v, &idx).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert_eq!(r["id"], format!("Journal/{today}"));
    assert_eq!(r["created"], true);
    let text = read(v.join("Journal").join(format!("{today}.md")));
    assert_eq!(text, format!("# {today}\ncreated {}\n", &today[..4]));
    // a second call opens the same note without touching it
    fs::write(v.join("Journal").join(format!("{today}.md")), "mine").unwrap();
    assert_eq!(fastobsidian::daily::daily_note(&v, &idx).unwrap()["created"], false);
    assert_eq!(read(v.join("Journal").join(format!("{today}.md"))), "mine");
}

#[test]
fn attachments_follow_the_folder_setting_and_never_overwrite() {
    let v = make_vault(&[(".obsidian/app.json", r#"{"attachmentFolderPath": "./assets"}"#), ("Notes/A.md", "a")]);
    let idx = VaultIndex::new(&v);
    let save = |name: &str| fastobsidian::daily::save_attachment(&v, &idx, "Notes/A", name, b"PNG");
    let r = save("shot.png").unwrap();
    assert_eq!(r["path"], "Notes/assets/shot.png");
    assert_eq!(r["embed"], "shot.png");
    assert_eq!(save("shot.png").unwrap()["path"], "Notes/assets/shot 1.png");
    assert_eq!(fs::read(v.join("Notes/assets/shot.png")).unwrap(), b"PNG");
    assert!(save("evil.exe").is_err());
    assert!(save("noext").is_err());
    assert_eq!(save("../../x.png").unwrap()["path"], "Notes/assets/x.png");
}

#[test]
fn index_cache_round_trips_exactly() {
    use fastobsidian::{cache, vault};
    let v = make_vault(&[
        ("A.md", "---\ntags: [x]\naliases: [Alpha]\nn: 1.5\n---\n# A\nsee [[B]] and [c](C.md) #t\n"),
        ("sub/B.md", "Ünïcode [[A|a]] ```#no``` `#no` #yes\n"),
        ("empty.md", ""),
    ]);
    let (md, _) = vault::walk_vault(&v);
    let fresh: Vec<_> = md
        .iter()
        .map(|f| {
            (f.rel.clone(), f.mtime_ns, f.size, Arc::new(vault::parse_note(&f.rel, f.mtime_ns, &read(f.path.clone()))))
        })
        .collect();
    let file = v.join("cache.idx");
    cache::save(&file, &v, &fresh).unwrap();
    let loaded = cache::load(&file, &v).expect("cache loads");
    assert_eq!(loaded.len(), fresh.len());
    for (rel, m, s, n) in &fresh {
        let (lm, ls, ln) = &loaded[rel];
        assert_eq!((lm, ls), (m, s));
        assert_eq!(format!("{ln:?}"), format!("{n:?}"), "{rel}");
    }
    // another vault, or a damaged file, is not used
    assert!(cache::load(&file, &v.join("sub")).is_none());
    let mut bytes = fs::read(&file).unwrap();
    bytes.truncate(bytes.len() - 3);
    fs::write(&file, bytes).unwrap();
    assert!(cache::load(&file, &v).is_none());
}
