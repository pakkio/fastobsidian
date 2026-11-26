# fastobsidian

A quick explorer and editor for an [Obsidian](https://obsidian.md) vault, in its own window.
One Rust program: it opens the vault in a native window that uses the system's web view
(WebView2 on Windows, WebKit on macOS and Linux), so no external browser is involved. No network calls.

The original Python version (`obsidian_explorer.py`, standard library only) is still here and works the same way
in a browser; see [Python version](#python-version).

## Features

- **Index** – stats, tag cloud, most connected / recently modified notes, orphans, unresolved links, sortable A–Z table
- **Graph** – force-directed canvas: pan, zoom, drag, hover highlight, click to open, focus on a note (1–3 hops), colour by cluster or folder, filter by tag, spread slider, unresolved links as ghost nodes
- **Search** – folder tree plus full-text search with snippets, ranked by name, alias, tag, then body; click a tag to filter
- **Notes** – rendered markdown (headings, lists, tasks, tables, code, callouts, highlights, tags), backlinks and outgoing links
- **Navigation** – both `[[wikilinks]]` (with `#headings`, `|display` text, and frontmatter `aliases` as link targets) and standard `[text](path/note.md)` links, relative or vault-root
- **Focus mode** – Ctrl+F on a note hides the columns and goes full screen; press again (or Esc) to restore them
- **Table of contents** – clickable per-note TOC that tracks your scroll position
- **Theme** – glass-style UI with a light/dark toggle (follows the system by default, choice is remembered);
  `--theme` applies the vault's own Obsidian theme and enabled CSS snippets instead
- **Large vaults** – incremental index that re-reads only changed files; search runs in the app
- **Live refresh** – notes edited outside the app (e.g. in Obsidian) update in the open window within about a second
- **Editing** – create, edit, rename and delete notes with a split markdown editor and live preview
- **Vault switching** – Ctrl+O opens another vault (native folder dialog) without restarting
- **MCP server** – `--mcp` exposes search, read, write/append, index, backlinks, rename and delete to AI agents over stdio

## Build and run

Requires a recent stable [Rust](https://rustup.rs). On Linux also the WebKitGTK development packages
(Debian/Ubuntu: `sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev`). Windows 10/11 already has WebView2.

```bash
cargo build --release            # -> target/release/fastobsidian(.exe), a single file

fastobsidian /path/to/vault      # open the vault in the app window
fastobsidian                     # no vault: a folder dialog asks for one
```

The binary can be copied anywhere: the page is built into it. Other modes of the same program:

```bash
fastobsidian /path/to/vault --serve            # serve at http://127.0.0.1:8765/ for any browser (headless)
fastobsidian /path/to/vault -o explorer.html   # write a self-contained read-only page
fastobsidian /path/to/vault --mcp              # MCP server on stdio
```

Options: `--theme` (use the vault's Obsidian theme, see below), `--watch SECONDS` (how often the vault is checked for
outside changes while a window or page is open, default 1; `0` turns it off), `--port N` (for `--serve`, default 8765),
`-o FILE` (static output), `--help`, `--version`. Try it with the bundled `sample-vault/`.

### How the window works

The app starts the same local server as `--serve` on `127.0.0.1` (port 47615, or a free one if that is taken) and shows
it in a window with the system web view. The fixed port keeps the page's origin the same between launches, so the
light/dark choice is remembered. Web and mail links open in your default browser, attachments (PDF, images, …) in their
own app. Closing the window with unsaved editor text asks first. The web view keeps its data in
`%LOCALAPPDATA%\fastobsidian` (Windows), `~/Library/Application Support/fastobsidian` (macOS) or
`~/.local/share/fastobsidian` (Linux).

## Obsidian themes and snippets

With `--theme`, the theme and CSS snippets enabled in `<vault>/.obsidian/appearance.json` are added to the page
(`.obsidian/themes/<Name>/theme.css` and `.obsidian/snippets/<name>.css`), and the explorer's colours follow the theme's
Obsidian variables (`--background-primary`, `--text-normal`, `--interactive-accent`, …). The light/dark toggle still works,
and its default comes from Obsidian's base theme. Only colours are mapped: a theme's layout rules target Obsidian's own
interface, so they mostly have no effect here. A theme can load fonts or images from the internet, so with `--theme` the
page may make network requests. It is off by default.

## Live refresh

An open window or page listens on `/api/events`, and the vault is checked every `--watch` seconds while one is open
(no checks when none is). When something changes on disk, the index, sidebar and graph reload and the open note is
re-rendered at the same scroll position. In the editor, a note without local changes takes the new text; with unsaved
changes, your text is kept and a warning says the file changed on disk (saving would be refused anyway).

## MCP server (for AI agents)

`fastobsidian /path/to/vault --mcp` speaks MCP over stdio. Tools: `search` (ranked: name > alias > tag > body, optional `tag`),
`read`, `write` (`append` default, `overwrite`, `create`; `tags`/`aliases` become frontmatter on new notes), `index`, `backlinks`, `rename`, `delete` (to `.trash`).

```json
{ "mcpServers": { "vault": { "command": "/path/to/fastobsidian", "args": ["/path/to/vault", "--mcp"] } } }
```

Frontmatter `aliases` also resolve `[[alias]]` links.

## Editing

Each note has **edit**, **rename** and **delete** buttons, and the sidebar has **+ New note**
(path like `Folder/Name`). Ctrl+S saves; Ctrl+O switches to another vault; a toolbar inserts bold, italic, headings, lists, tasks, `[[links]]`, code and tags.

- **Delete is recoverable**: files move to `<vault>/.trash/`.
- **Rename updates links**: `[[wikilinks]]` and markdown `[text](x.md)` links in other notes are repointed
  (markdown links are rewritten relative to each note; a moved note's own relative links are fixed too).
  The Rust version keeps each note's line endings when it rewrites links.
- **Conflict check**: saving is refused if the file changed on disk since you opened it.
- Saving from the editor writes LF line endings.

## Security

The server binds to `127.0.0.1` only, rejects unexpected `Host`/`Origin` headers, requires a custom header on writes,
and only touches `.md` files inside the vault (paths with `..` or dot-folders are refused).
It has no authentication, so do not expose it on a network.

Symlinks and Windows junctions are never followed: linked files and folders are left out of the index, and reads,
writes, renames and attachment requests through a link are refused, so a link inside the vault cannot redirect a write
to a file elsewhere. (The check runs just before each file operation; it is not atomic.) Notes over 32 MiB are left out
of the index and refused by read and write (`MAX_NOTE_BYTES`).

## Notes

- Folders starting with `.` (e.g. `.obsidian`, `.trash`) are ignored.
- Attachments (images, PDF, audio, video) are detected and resolved by the index; the file type list is `MEDIA_EXT` in `src/vault.rs`.
- `explorer.html` embeds your vault's content; it is gitignored by default, so do not commit or share it unintentionally.

## Python version

`obsidian_explorer.py` needs only Python 3.9+ and runs in your browser instead of a window. Same options, plus `--open`
to launch the browser; no `pick folder` dialog (Ctrl+O asks for a path).

```bash
python obsidian_explorer.py /path/to/vault --serve --open      # http://127.0.0.1:8765/
python obsidian_explorer.py /path/to/vault -o explorer.html --open
python obsidian_explorer.py /path/to/vault --mcp
```

## Files

| File | Purpose |
|---|---|
| `src/main.rs`, `src/gui.rs` | Command line and the app window |
| `src/vault.rs` | Parsing, incremental index, search |
| `src/ops.rs` | Safe paths, read/save/create/rename (link rewriting)/trash |
| `src/server.rs`, `src/page.rs`, `src/mcp.rs` | Local server and live events, page and themes, MCP server |
| `explorer_template.html` | UI (index, graph, note view, editor), built into the binary and used by the Python version |
| `obsidian_explorer.py` | Python version |
| `sample-vault/` | Demo notes |
| `tests/` | `cargo test` (Rust) and `python -m unittest discover -s tests` (Python), run by GitHub Actions on Linux, Windows and macOS |

## License

MIT, see [LICENSE](LICENSE).
