#!/usr/bin/env python3
"""Generate a single self-contained HTML explorer (index, search, backlinks, graph)
for an Obsidian vault.

    python obsidian_explorer.py [VAULT_DIR] [-o explorer.html] [--open]
"""
import argparse
import json
import os
import re
import shutil
import sys
import webbrowser
from datetime import datetime
from pathlib import Path
from urllib.parse import unquote

FENCE = re.compile(r"(```|~~~).*?\1", re.S)
INLINE_CODE = re.compile(r"`[^`\n]*`")
WIKI = re.compile(r"(!?)\[\[([^\]|#]*)(#[^\]|]*)?(?:\|[^\]]*)?\]\]")
TAG = re.compile(r"(?<![\w/&#])#([A-Za-z_][\w/-]*)")
FM = re.compile(r"---[ \t]*\r?\n(.*?)\r?\n---[ \t]*(?:\r?\n|$)", re.S)
MEDIA_EXT = {"png", "jpg", "jpeg", "gif", "svg", "webp", "bmp", "pdf", "mp3", "mp4",
             "wav", "ogg", "webm", "mov", "m4a"}
ASSET_EXT = MEDIA_EXT | {"canvas", "base"}
# notes larger than this are left out of the index and refused by read/write: a huge .md must not eat all memory
MAX_NOTE_BYTES = 32 * 1024 * 1024


def _is_link(path) -> bool:
    """True for a symlink, or a Windows junction (os.path.isjunction exists from Python 3.12)."""
    isjunction = getattr(os.path, "isjunction", None)
    return os.path.islink(path) or bool(isjunction and isjunction(path))


def no_links(vault: Path, p: Path) -> Path:
    """Raise ValueError if `p`, or any folder between the vault and `p`, is a symlink or junction.

    Reads and writes never follow links, so a link inside the vault cannot redirect them to a file elsewhere."""
    cur = vault
    for part in p.relative_to(vault).parts:
        cur = cur / part
        if _is_link(cur):
            raise ValueError(f"refusing to follow a symlink: {cur.relative_to(vault).as_posix()}")
    return p


def walk_vault(vault: Path):
    """(md, media): md = sorted [(rel, path, mtime_ns, size)], media = [rel].

    Skips dot-folders, symlinks/junctions (files and folders) and notes over MAX_NOTE_BYTES."""
    md, media, stack = [], [], [(str(vault), "")]
    while stack:
        path, rel = stack.pop()
        try:
            it = os.scandir(path)
        except OSError:
            continue
        with it:
            for e in it:
                if e.name.startswith("."):
                    continue
                r = f"{rel}/{e.name}" if rel else e.name
                try:
                    is_junction = getattr(e, "is_junction", None)
                    if e.is_symlink() or (is_junction and is_junction()):
                        continue
                    if e.is_dir(follow_symlinks=False):
                        stack.append((e.path, r))
                        continue
                    if not e.is_file(follow_symlinks=False):
                        continue
                    ext = e.name.rsplit(".", 1)[-1].lower() if "." in e.name else ""
                    if ext == "md":
                        st = e.stat(follow_symlinks=False)   # free on Windows (comes from the directory listing)
                        if st.st_size <= MAX_NOTE_BYTES:
                            md.append((r, e.path, st.st_mtime_ns, st.st_size))
                    elif ext in MEDIA_EXT:
                        media.append(r)
                except OSError:
                    continue
    md.sort()
    return md, media


# [text](note.md#h) or [text](<my note.md#h>) -- images (![..]) excluded
MDLINK = re.compile(r"(?<!!)\[[^\]]*\]\((?:<([^>]+?\.md)(?:#[^>]*)?>|([^)\s#]+?\.md)(?:#[^)\s]*)?)(?:\s+\"[^\"]*\")?\)", re.I)


def resolve_md(raw, from_id, lookup):
    """Resolve a markdown-link target (relative to the note, or vault-root) to a note id."""
    if re.match(r"[a-z][a-z0-9+.-]*:", raw, re.I):  # http:, mailto:, ...
        return None
    target = unquote(raw)[:-3]
    base = from_id.rsplit("/", 1)[0].split("/") if "/" in from_id else []
    parts = [] if target.startswith("/") else base[:]
    for seg in target.lstrip("/").split("/"):
        if seg == "..":
            if parts:
                parts.pop()
        elif seg not in ("", "."):
            parts.append(seg)
    return lookup.get("/".join(parts).lower()) or lookup.get(target.lstrip("/").lower())


def parse_frontmatter(text):
    m = FM.match(text) if text.startswith("---") else None
    if not m:
        return {}, text
    fm, key = {}, None
    for line in m.group(1).splitlines():
        item = re.match(r"\s*-\s+(.*)", line)
        kv = re.match(r"([\w-]+):\s*(.*)", line)
        if kv:
            key, val = kv.group(1).lower(), kv.group(2).strip()
            if val.startswith("[") and val.endswith("]"):
                val = [v.strip().strip("'\"") for v in val[1:-1].split(",") if v.strip()]
            elif val and key in ("tags", "tag", "aliases"):
                val = [v.strip().strip("'\"") for v in re.split(r"[,\s]+", val) if v.strip()]
            fm[key] = val if val != "" else []
        elif item and key and isinstance(fm.get(key), list):
            fm[key].append(item.group(1).strip().strip("'\""))
    return fm, text[m.end():]


def note_aliases(p) -> list:
    v = p["props"].get("aliases") or p["props"].get("alias") or []
    return [str(a) for a in (v if isinstance(v, list) else [v]) if str(a).strip()]


def parse_note(rel: str, path: str, mtime_ns: int, text: str) -> dict:
    """Parse one note. Link *targets* are extracted here; resolving them needs the whole vault (see VaultIndex)."""
    nid = rel[:-3]
    fm, body = parse_frontmatter(text)
    stripped = INLINE_CODE.sub("", FENCE.sub("", body))
    tags = set()
    for k in ("tags", "tag"):
        v = fm.get(k, [])
        tags.update(str(t).lstrip("#") for t in (v if isinstance(v, list) else [v]) if t)
    tags |= set(TAG.findall(stripped))
    content = body.strip()
    return {
        "id": nid, "title": nid.rsplit("/", 1)[-1], "folder": nid.rsplit("/", 1)[0] if "/" in nid else "",
        "content": content, "lc": content.lower(), "mtime": mtime_ns // 1_000_000_000,
        "rev": str(mtime_ns),  # exact change marker for the browser's content cache (a string: ns overflow JS numbers)
        "words": len(re.findall(r"\w+", body)), "tags": sorted(tags),
        "props": {k: v for k, v in fm.items() if k not in ("tags", "tag")},
        "wiki": [t.strip() for _, t, _ in WIKI.findall(stripped) if t.strip()],
        "mdl": [m.group(1) or m.group(2) for m in MDLINK.finditer(stripped)],
    }


class VaultIndex:
    """Incremental vault index: re-reads only files whose mtime/size changed; caches the resolved result."""

    MIN_RECHECK = 2.0

    def __init__(self, vault: Path):
        import threading
        self.vault = vault
        self.files = {}          # rel -> (mtime_ns, size, parsed)
        self.sig = None
        self.result = None       # data without note content
        self.notes = {}          # id -> parsed (with content)
        self.lock = threading.Lock()
        self.checked = 0.0       # last time the folder tree was walked
        self.stale = True
        self.version = 0         # bumped every time the vault content changes (the watcher pushes it to browsers)

    def invalidate(self):
        """Call after writing through the server so the next build re-walks immediately."""
        self.stale = True

    def build(self, force=False):
        """Current index. Without `force`, a walk happens at most every MIN_RECHECK seconds (or after invalidate)."""
        import time
        with self.lock:
            # external edits (e.g. in Obsidian) are picked up within MIN_RECHECK seconds; our own writes invalidate
            if (not force and self.result is not None and not self.stale
                    and time.monotonic() - self.checked < self.MIN_RECHECK):
                return self.result
            md, media = walk_vault(self.vault)
            self.checked, self.stale = time.monotonic(), False
            sig = hash((tuple((r, m, s) for r, _, m, s in md), tuple(sorted(media))))
            if sig == self.sig and self.result is not None:
                return self.result
            files = {}
            for r, path, m, s in md:
                old = self.files.get(r)
                if old and old[0] == m and old[1] == s:
                    files[r] = old
                    continue
                try:
                    with open(path, encoding="utf-8", errors="replace") as f:
                        files[r] = (m, s, parse_note(r, path, m, f.read()))
                except OSError:
                    pass
            self.files = files
            self.notes = {p["id"]: p for _, _, p in files.values()}
            self.result = self._resolve(media)
            self.sig = sig
            self.version += 1
            return self.result

    def _resolve(self, media):
        parsed = [p for _, _, p in self.files.values()]
        lookup = {}
        for p in sorted(parsed, key=lambda p: (p["id"].count("/"), p["id"])):
            parts = p["id"].split("/")
            for i in range(len(parts)):
                lookup.setdefault("/".join(parts[i:]).lower(), p["id"])
        for p in sorted(parsed, key=lambda p: p["id"]):  # aliases come last: real note names win
            for a in note_aliases(p):
                lookup.setdefault(a.lower(), p["id"])
        unresolved, notes = {}, []
        for p in sorted(parsed, key=lambda p: p["id"]):
            links, seen = [], set()

            def link(dest, missing):
                if dest:
                    if dest != p["id"] and dest not in seen:
                        seen.add(dest)
                        links.append(dest)
                elif missing:
                    src = unresolved.setdefault(missing, [])
                    if not src or src[-1] != p["id"]:
                        src.append(p["id"])

            for target in p["wiki"]:
                key = re.sub(r"\.md$", "", target, flags=re.I).lower()
                ext = key.rsplit(".", 1)[-1] if "." in key else ""
                link(lookup.get(key), None if ext in ASSET_EXT else target)
            for raw in p["mdl"]:
                ext_url = re.match(r"[a-z][a-z0-9+.-]*:", raw, re.I)
                link(None if ext_url else resolve_md(raw, p["id"], lookup),
                     None if ext_url else unquote(raw).rsplit("/", 1)[-1][:-3])
            notes.append({k: p[k] for k in ("id", "title", "folder", "mtime", "rev", "words", "tags", "props")} | {"links": links})
        assets = {}
        for rel in sorted(media, key=lambda r: (r.count("/"), r)):
            parts = rel.split("/")
            for i in range(len(parts)):
                assets.setdefault("/".join(parts[i:]).lower(), rel)
        return {
            "vault": self.vault.name or str(self.vault), "generated": datetime.now().isoformat(timespec="seconds"),
            "notes": notes, "lookup": lookup, "assets": assets,
            "unresolved": [{"target": t, "sources": s} for t, s in sorted(unresolved.items())],
        }

    def json(self):
        """Serialized result, cached until the vault changes."""
        res = self.build()
        if getattr(self, "_json_for", None) is not res:
            self._json, self._json_for = json.dumps(res, ensure_ascii=False), res
        return self._json

    def contents(self, ids):
        self.build()
        return {i: self.notes[i]["content"] for i in ids if i in self.notes}

    def search(self, q, limit=300, tag=""):
        """Ranked search: name > alias > tag > body. `tag` restricts to notes carrying that tag."""
        self.build()
        ql, tag, out = q.lower().strip(), tag.lower().strip().lstrip("#"), []
        if not ql and not tag:
            return out
        for nid, p in self.notes.items():
            tags = [t.lower() for t in p["tags"]]
            if tag and tag not in tags:
                continue
            c, score, i = p["content"], 0, p["lc"].find(ql) if ql else -1
            if ql:
                title = p["title"].lower()
                if title == ql:
                    score = 120
                elif ql in nid.lower():
                    score = 100
                elif any(ql in a.lower() for a in note_aliases(p)):
                    score = 80
                elif any(ql in t for t in tags):
                    score = 60
                elif i >= 0:
                    score = 10
                if not score:
                    continue
            snip = c[max(0, i - 40):i + 80] if i >= 0 else c[:120]
            out.append({"id": nid, "snippet": re.sub(r"\s+", " ", snip), "score": score})
        out.sort(key=lambda r: (-r["score"], r["id"]))
        return out[:limit]


def build(vault: Path, with_content: bool = True) -> dict:
    """One-shot build (static mode): note content is embedded."""
    idx = VaultIndex(vault)
    data = dict(idx.build())
    if with_content:
        data["notes"] = [n | {"content": idx.notes[n["id"]]["content"]} for n in data["notes"]]
    return data


BAD_CHARS = re.compile(r'[<>:"|?*\x00-\x1f\\]')


def note_path(vault: Path, nid: str) -> Path:
    """Map a note id ('Folder/Name') to a .md path inside the vault, or raise ValueError."""
    nid = nid.strip().strip("/")
    if nid.lower().endswith(".md"):
        nid = nid[:-3]
    segs = nid.split("/")
    if not nid or any(not s.strip() or s.startswith(".") or s.endswith(".") or BAD_CHARS.search(s) for s in segs):
        raise ValueError(f"invalid note name: {nid!r}")
    p = vault / (nid + ".md")
    if vault not in p.resolve().parents:
        raise ValueError("path escapes the vault")
    return no_links(vault, p)


def rewrite_links(vault: Path, old_id: str, new_id: str, lookup: dict) -> int:
    """After a rename, repoint [[old]] / [[folder/old]] wikilinks and [text](old.md) links.

    `lookup` must be the note lookup from *before* the rename (to resolve markdown links).
    Only notes the index sees are touched: no dot-folders, no symlinks, nothing over MAX_NOTE_BYTES."""
    old_base, new_base = old_id.rsplit("/", 1)[-1], new_id.rsplit("/", 1)[-1]
    pairs = [(old_id, new_id)] + ([(old_base, new_base)] if old_base != new_base else [])
    changed = 0
    for rel_s, path, _, _ in walk_vault(vault)[0]:
        p, me_new = Path(path), rel_s[:-3]
        # a renamed note keeps its own relative links pointing at the old location, so use old id for resolution
        me_old = old_id if me_new == new_id else me_new
        text = p.read_text(encoding="utf-8", errors="replace")
        new = text
        for old, rep in pairs:
            new = re.sub(r"(\[\[)" + re.escape(old) + r"(?=[\]|#])", lambda m: m.group(1) + rep, new, flags=re.I)

        def fix(m):
            raw = m.group(1) or m.group(2)
            dest = resolve_md(raw, me_old, lookup)  # raw excludes any #fragment, which is left untouched
            if dest is None:
                return m.group(0)
            moved_dir = os.path.dirname(old_id) != os.path.dirname(new_id)
            if dest != old_id and not (me_new == new_id and moved_dir):
                return m.group(0)  # unrelated link: leave its style alone
            dest = new_id if dest == old_id else dest
            target = os.path.relpath(dest, os.path.dirname(me_new) or ".").replace("\\", "/") + ".md"
            if m.group(2) and " " in target:
                target = target.replace(" ", "%20")
            return m.group(0).replace(raw, target, 1) if unquote(target) != unquote(raw) else m.group(0)
        new = MDLINK.sub(fix, new)
        if new != text:
            p.write_text(new, encoding="utf-8")
            changed += 1
    return changed


class Conflict(Exception):
    """The note changed on disk since the caller read it."""


def _check_size(p: Path, extra: int = 0):
    if p.stat().st_size + extra > MAX_NOTE_BYTES:
        raise ValueError(f"note is larger than {MAX_NOTE_BYTES >> 20} MiB")


def read_note(vault: Path, nid: str) -> dict:
    p = note_path(vault, nid)
    if not p.is_file():
        raise FileNotFoundError
    _check_size(p)
    return {"raw": p.read_text(encoding="utf-8", errors="replace"), "mtime": p.stat().st_mtime_ns}


def save_note(vault: Path, idx: "VaultIndex", nid: str, raw: str, mtime=None, append=False) -> dict:
    p = note_path(vault, nid)
    if not p.is_file():
        raise FileNotFoundError
    if mtime and p.stat().st_mtime_ns != mtime:
        raise Conflict("file changed on disk since you opened it")
    data = raw.encode("utf-8")
    if len(data) > MAX_NOTE_BYTES:
        raise ValueError(f"note is larger than {MAX_NOTE_BYTES >> 20} MiB")
    if append:
        _check_size(p, len(data))
        old = p.read_bytes().decode("utf-8", errors="replace")
        raw = old + ("" if not old or old.endswith("\n") else "\n") + raw
    p.write_bytes(raw.encode("utf-8"))
    idx.invalidate()
    return {"ok": True, "mtime": p.stat().st_mtime_ns}


def create_note(vault: Path, idx: "VaultIndex", nid: str, raw: str) -> dict:
    p = note_path(vault, nid)
    if p.exists() or p.is_symlink():
        raise ValueError("a note with that name already exists")
    if len(raw.encode("utf-8")) > MAX_NOTE_BYTES:
        raise ValueError(f"note is larger than {MAX_NOTE_BYTES >> 20} MiB")
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_bytes(raw.encode("utf-8"))
    idx.invalidate()
    return {"ok": True, "mtime": p.stat().st_mtime_ns}


def rename_note(vault: Path, idx: "VaultIndex", nid: str, new: str) -> dict:
    src, dst = note_path(vault, nid), note_path(vault, new)
    if not src.is_file():
        raise FileNotFoundError
    if (dst.exists() or dst.is_symlink()) and dst != src:
        raise ValueError("target already exists")
    old_lookup = idx.build()["lookup"]
    dst.parent.mkdir(parents=True, exist_ok=True)
    os.replace(src, dst)
    n = rewrite_links(vault, nid.strip("/").removesuffix(".md"), new.strip("/").removesuffix(".md"), old_lookup)
    idx.invalidate()
    return {"ok": True, "updated": n}


def trash_note(vault: Path, idx: "VaultIndex", nid: str) -> dict:
    """Move to <vault>/.trash (recoverable)."""
    import time
    p = note_path(vault, nid)
    if not p.is_file():
        raise FileNotFoundError
    trash = no_links(vault, vault / ".trash")
    trash.mkdir(exist_ok=True)
    dst = trash / p.name
    if dst.exists():
        dst = trash / f"{p.stem}-{int(time.time())}{p.suffix}"
    shutil.move(str(p), str(dst))
    idx.invalidate()
    return {"ok": True, "trashed": dst.name}


SAFE_NAME = re.compile(r"^[^/\\\x00-\x1f]+$")


def obsidian_css(vault: Path) -> dict:
    """The theme and CSS snippets enabled in <vault>/.obsidian/appearance.json.

    Returns {"css": str, "mode": "dark" | "light" | None, "theme": str}. Missing files are skipped."""
    base = vault / ".obsidian"
    try:
        app = json.loads(no_links(vault, base / "appearance.json").read_text(encoding="utf-8"))
        if not isinstance(app, dict):
            app = {}
    except (OSError, ValueError):
        app = {}

    def read(p: Path):
        try:
            no_links(vault, p)
            if p.is_file() and p.stat().st_size <= MAX_NOTE_BYTES:
                return p.read_text(encoding="utf-8", errors="replace")
        except (OSError, ValueError):
            pass
        return None

    parts, theme = [], str(app.get("cssTheme") or "")
    if theme and SAFE_NAME.match(theme) and theme not in (".", ".."):
        for p in (base / "themes" / theme / "theme.css", base / "themes" / f"{theme}.css"):  # current / pre-1.0 layout
            css = read(p)
            if css is not None:
                parts.append(f"/* theme: {theme} */\n{css}")
                break
    snippets = app.get("enabledCssSnippets") or []
    for s in snippets if isinstance(snippets, list) else []:
        if isinstance(s, str) and SAFE_NAME.match(s) and s not in (".", ".."):
            css = read(base / "snippets" / f"{s}.css")
            if css is not None:
                parts.append(f"/* snippet: {s} */\n{css}")
    mode = {"obsidian": "dark", "moonstone": "light"}.get(app.get("theme"))
    return {"css": "\n".join(parts), "mode": mode, "theme": theme}


PLACEHOLDER = re.compile(r"/\*__(\w+)__\*/(?:false|null)?")


def page(vault: Path, live: bool, index: "VaultIndex | None" = None, theme: bool = False) -> str:
    """Fill the template in one pass, so text inserted from the vault is never scanned for placeholders."""
    tpl = (Path(__file__).parent / "explorer_template.html").read_text(encoding="utf-8")
    raw = index.json() if index else json.dumps(build(vault), ensure_ascii=False)
    obs = obsidian_css(vault) if theme else None
    values = {
        "LIVE": "true" if live else "false",
        "DATA": raw.replace("</", "<\\/"),
        "OBS": json.dumps({"mode": obs["mode"], "theme": obs["theme"]}).replace("</", "<\\/") if obs else "null",
        "OBSCSS": re.sub(r"</(style)", r"<\\/\1", obs["css"], flags=re.I) if obs else "",
    }
    return PLACEHOLDER.sub(lambda m: values.get(m.group(1), m.group(0)), tpl)


def make_server(vault: Path, port: int, theme: bool = False, watch: float = 1.0):
    """Build (not start) the local server. `watch` = seconds between vault checks while a browser listens (0 = off)."""
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from urllib.parse import urlparse, parse_qs
    import threading
    import time

    idx = VaultIndex(vault)
    t0 = time.time()
    n = len(idx.build()["notes"])
    print(f"indexed {n} notes in {time.time() - t0:.1f}s")
    allowed_hosts = set()   # filled in once the port is known (port 0 = any free port)
    events = threading.Condition()
    state = {"v": 0, "clients": 0}

    def watcher():
        """Re-check the vault while at least one browser is listening on /api/events; announce every change."""
        last = (id(idx), idx.version)
        while True:
            time.sleep(watch)
            if not state["clients"]:
                continue
            cur = idx
            try:
                cur.build(force=True)
            except Exception as e:  # noqa: BLE001 -- a bad file must not kill the watcher
                print(f"watcher: {type(e).__name__}: {e}", file=sys.stderr)
                continue
            key = (id(cur), cur.version)
            if key != last:
                last = key
                with events:
                    state["v"] += 1
                    events.notify_all()

    def switch(path):
        nonlocal vault, idx
        new = Path(path).expanduser().resolve()
        if not new.is_dir():
            raise ValueError(f"not a directory: {new}")
        vault, idx = new, VaultIndex(new)
        idx.build()

    class H(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def send(self, code, body, ctype="application/json"):
            b = body if isinstance(body, bytes) else (body if isinstance(body, str) and ctype != "application/json" else json.dumps(body)).encode("utf-8")
            self.send_response(code)
            textual = ctype.startswith("text/") or ctype == "application/json"
            self.send_header("Content-Type", ctype + ("; charset=utf-8" if textual else ""))
            if not textual:
                self.send_header("X-Content-Type-Options", "nosniff")
            if ctype == "image/svg+xml":  # an SVG opened directly must not run script in our origin
                self.send_header("Content-Security-Policy", "sandbox; default-src 'none'; style-src 'unsafe-inline'")
            self.send_header("Content-Length", str(len(b)))
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(b)

        def guard(self, write):
            if self.headers.get("Host") not in allowed_hosts:  # DNS-rebinding protection
                self.send(403, {"error": "bad host"})
                return False
            if write:
                origin = self.headers.get("Origin")
                if self.headers.get("X-Explorer") != "1" or (origin and urlparse(origin).netloc not in allowed_hosts):
                    self.send(403, {"error": "forbidden"})
                    return False
            return True

        def body(self):
            n = int(self.headers.get("Content-Length") or 0)
            return json.loads(self.rfile.read(n) or b"{}")

        def stream_events(self):
            """Server-sent events: `hello` with the current version, then `change` whenever the vault changes."""
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            with events:
                state["clients"] += 1
                seen = state["v"]
            try:
                self.wfile.write(f"event: hello\ndata: {seen}\n\n".encode())
                self.wfile.flush()
                while True:
                    with events:
                        if state["v"] == seen:
                            events.wait(15)
                        v = state["v"]
                    if v != seen:
                        seen = v
                        self.wfile.write(f"event: change\ndata: {v}\n\n".encode())
                    else:
                        self.wfile.write(b": ping\n\n")  # keeps proxies happy and detects closed tabs
                    self.wfile.flush()
            except OSError:  # browser tab closed
                pass
            finally:
                with events:
                    state["clients"] -= 1

        def dispatch(self, fn, write):
            if not self.guard(write):
                return
            try:
                url = urlparse(self.path)
                fn(url.path, {k: v[0] for k, v in parse_qs(url.query).items()})
            except Conflict as e:
                self.send(409, {"error": str(e)})
            except ValueError as e:
                self.send(400, {"error": str(e)})
            except FileNotFoundError:
                self.send(404, {"error": "not found"})
            except Exception as e:  # noqa: BLE001
                self.send(500, {"error": f"{type(e).__name__}: {e}"})

        def do_GET(self):
            def go(path, q):
                if path == "/":
                    self.send(200, page(vault, True, idx, theme), "text/html")
                elif path == "/api/events":
                    self.stream_events()
                elif path == "/api/data":
                    self.send(200, idx.json().encode("utf-8"))
                elif path == "/api/vault":
                    self.send(200, {"path": str(vault)})
                elif path == "/api/search":
                    self.send(200, idx.search(q.get("q", ""), tag=q.get("tag", "")))
                elif path.startswith("/vault/"):
                    rel = unquote(path[len("/vault/"):])
                    segs = rel.split("/")
                    f = vault / rel
                    if (any(not s or s.startswith(".") for s in segs) or f.suffix.lower()[1:] not in MEDIA_EXT
                            or vault not in f.resolve().parents or not f.is_file()):
                        raise FileNotFoundError
                    no_links(vault, f)
                    import mimetypes
                    self.send(200, f.read_bytes(), (mimetypes.guess_type(f.name)[0] or "application/octet-stream"))
                elif path == "/api/note":
                    self.send(200, read_note(vault, q.get("id", "")))
                else:
                    self.send(404, {"error": "not found"})
            self.dispatch(go, False)

        def do_PUT(self):  # save existing
            def go(path, q):
                d = self.body()
                self.send(200, save_note(vault, idx, d.get("id", ""), d.get("raw", ""), d.get("mtime")))
            self.dispatch(go, True)

        def do_POST(self):  # create / rename
            def go(path, q):
                d = self.body()
                if path == "/api/contents":  # read-only; POST only to carry a long id list
                    ids = d.get("ids", [])
                    if not isinstance(ids, list) or len(ids) > 2000:
                        raise ValueError("bad ids")
                    self.send(200, idx.contents([str(i) for i in ids]))
                elif path == "/api/note":
                    self.send(200, create_note(vault, idx, d.get("id", ""), d.get("raw", "")))
                elif path == "/api/vault":
                    switch(str(d.get("path", "")).strip().strip('"'))
                    self.send(200, {"ok": True, "path": str(vault)})
                elif path == "/api/rename":
                    self.send(200, rename_note(vault, idx, d.get("id", ""), d.get("new", "")))
                else:
                    self.send(404, {"error": "not found"})
            self.dispatch(go, True)

        def do_DELETE(self):  # move to <vault>/.trash (recoverable)
            def go(path, q):
                self.send(200, trash_note(vault, idx, q.get("id", "")))
            self.dispatch(go, True)

    srv = ThreadingHTTPServer(("127.0.0.1", port), H)
    srv.daemon_threads = True
    port = srv.server_address[1]
    allowed_hosts.update({f"127.0.0.1:{port}", f"localhost:{port}"})
    if watch > 0:
        threading.Thread(target=watcher, name="vault-watcher", daemon=True).start()
    return srv


def serve(vault: Path, port: int, open_browser: bool, theme: bool = False, watch: float = 1.0):
    srv = make_server(vault, port, theme, watch)
    url = f"http://127.0.0.1:{srv.server_address[1]}/"
    print(f"Serving {vault} at {url}  (Ctrl+C to stop; deleted notes go to .trash)")
    if open_browser:
        webbrowser.open(url)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


def _frontmatter(tags, aliases) -> str:
    lines = ["---"]
    for key, vals in (("tags", tags), ("aliases", aliases)):
        if vals:
            lines += [f"{key}:"] + [f"  - {v}" for v in vals]
    return "\n".join(lines + ["---", ""])


def mcp_tools(vault: Path, idx: "VaultIndex") -> dict:
    """name -> (description, JSON-schema properties, required, handler). Handlers return JSON-able data."""
    S, A = {"type": "string"}, {"type": "array", "items": {"type": "string"}}

    def search(a):
        return idx.search(a.get("query", ""), int(a.get("limit", 20)), a.get("tag", ""))

    def read(a):
        return read_note(vault, a["id"])["raw"]

    def write(a):
        nid, content, mode = a["id"], a.get("content", ""), a.get("mode", "append")
        if mode not in ("append", "overwrite", "create"):
            raise ValueError("mode must be append, overwrite or create")
        exists = note_path(vault, nid).is_file()
        if exists and mode == "create":
            raise ValueError("a note with that name already exists")
        if exists:
            return save_note(vault, idx, nid, content, append=(mode == "append"))
        if (a.get("tags") or a.get("aliases")) and not content.startswith("---"):
            content = _frontmatter(a.get("tags"), a.get("aliases")) + content
        return create_note(vault, idx, nid, content)

    def index(a):
        res = idx.build()
        tags, linked = {}, set()
        for n in res["notes"]:
            for t in n["tags"]:
                tags[t] = tags.get(t, 0) + 1
            if n["links"]:
                linked.add(n["id"])
                linked.update(n["links"])
        return {"notes": [{k: n[k] for k in ("id", "tags", "mtime")} for n in res["notes"]],
                "tags": dict(sorted(tags.items(), key=lambda kv: -kv[1])),
                "orphans": [n["id"] for n in res["notes"] if n["id"] not in linked],
                "unresolved": res["unresolved"]}

    def backlinks(a):
        nid = a["id"].strip("/").removesuffix(".md")
        return [n["id"] for n in idx.build()["notes"] if nid in n["links"]]

    def rename(a):
        return rename_note(vault, idx, a["id"], a["new"])

    def delete(a):
        return trash_note(vault, idx, a["id"])

    return {
        "search": ("Ranked search (name > alias > tag > body). Empty query with tag lists that tag's notes.",
                   {"query": S, "tag": S, "limit": {"type": "integer"}}, [], search),
        "read": ("Read a note's raw markdown by id, e.g. 'Folder/Name'.", {"id": S}, ["id"], read),
        "write": ("Create a note, or append to / overwrite an existing one. mode: append (default), overwrite, create. "
                  "tags/aliases become frontmatter on new notes.",
                  {"id": S, "content": S, "mode": S, "tags": A, "aliases": A}, ["id", "content"], write),
        "index": ("All note ids with tags, tag counts, orphan notes and unresolved links.", {}, [], index),
        "backlinks": ("Ids of notes that link to the given note.", {"id": S}, ["id"], backlinks),
        "rename": ("Rename/move a note and repoint wikilinks and markdown links.", {"id": S, "new": S}, ["id", "new"], rename),
        "delete": ("Delete a note (moved to .trash, recoverable).", {"id": S}, ["id"], delete),
    }


def run_mcp(vault: Path):
    """Minimal MCP server over stdio (newline-delimited JSON-RPC 2.0)."""
    idx = VaultIndex(vault)
    tools = mcp_tools(vault, idx)
    sys.stdin.reconfigure(encoding="utf-8")
    sys.stdout.reconfigure(encoding="utf-8")

    def reply(id_, result=None, error=None):
        msg = {"jsonrpc": "2.0", "id": id_}
        msg["error" if error else "result"] = error or result
        sys.stdout.write(json.dumps(msg, ensure_ascii=False) + "\n")
        sys.stdout.flush()

    for line in sys.stdin:
        if not line.strip():
            continue
        try:
            req = json.loads(line)
            method, id_, params = req.get("method"), req.get("id"), req.get("params") or {}
        except (ValueError, AttributeError):
            reply(None, error={"code": -32700, "message": "parse error"})
            continue
        if id_ is None:  # notification (e.g. notifications/initialized): no reply
            continue
        if method == "initialize":
            reply(id_, {"protocolVersion": params.get("protocolVersion", "2025-03-26"),
                        "capabilities": {"tools": {}}, "serverInfo": {"name": "fastobsidian", "version": "0.1"}})
        elif method == "ping":
            reply(id_, {})
        elif method == "tools/list":
            reply(id_, {"tools": [{"name": n, "description": d, "inputSchema": {"type": "object", "properties": p, "required": r}}
                                  for n, (d, p, r, _) in tools.items()]})
        elif method == "tools/call":
            tool = tools.get(params.get("name"))
            if not tool:
                reply(id_, error={"code": -32602, "message": f"unknown tool: {params.get('name')}"})
                continue
            try:
                out = tool[3](params.get("arguments") or {})
                text, bad = (out if isinstance(out, str) else json.dumps(out, ensure_ascii=False)), False
            except KeyError as e:
                text, bad = f"missing argument: {e}", True
            except FileNotFoundError:
                text, bad = "note not found", True
            except (ValueError, Conflict) as e:
                text, bad = str(e), True
            except Exception as e:  # noqa: BLE001
                text, bad = f"{type(e).__name__}: {e}", True
            reply(id_, {"content": [{"type": "text", "text": text}], "isError": bad})
        else:
            reply(id_, error={"code": -32601, "message": f"method not found: {method}"})


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--serve", action="store_true", help="run local server with note editing (create/edit/rename/delete)")
    ap.add_argument("--mcp", action="store_true", help="run an MCP server on stdio (search/read/write/index/backlinks/rename/delete)")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("vault", nargs="?", default=".", help="vault directory (default: .)")
    ap.add_argument("-o", "--out", default="explorer.html")
    ap.add_argument("--open", action="store_true", help="open in the default browser")
    ap.add_argument("--theme", action="store_true",
                    help="apply the vault's Obsidian theme and enabled CSS snippets (.obsidian/appearance.json)")
    ap.add_argument("--watch", type=float, default=1.0, metavar="SECONDS",
                    help="server mode: how often to check the vault for outside changes while a page is open (0 = off)")
    a = ap.parse_args()

    vault = Path(a.vault).expanduser().resolve()
    if not vault.is_dir():
        sys.exit(f"not a directory: {vault}")
    if a.mcp:
        return run_mcp(vault)
    if a.serve:
        return serve(vault, a.port, a.open, a.theme, a.watch)
    out = Path(a.out).resolve()
    out.write_text(page(vault, False, theme=a.theme), encoding="utf-8")
    print(f"static read-only explorer -> {out}")
    if a.open:
        webbrowser.open(out.as_uri())


if __name__ == "__main__":
    main()
