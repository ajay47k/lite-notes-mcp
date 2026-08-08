# lite-notes

A tiny always-on-top floating markdown viewer for Windows, driven by Claude through MCP.

Ask Claude to show a markdown file, and it opens in a small frosted-glass window that
floats above everything else — so your plan, spec, or notes stay visible while Claude
Desktop or your editor keeps the full screen.

No browser, no local server, no network. A single ~8 MB executable plus a zero-dependency
stdio MCP server.

<!-- ![lite-notes screenshot](docs/screenshot.png) -->

## Why

Reading a long markdown file inside a chat sidebar is cramped, and splitting the window
costs you half your screen. lite-notes detaches that content into a widget you can park
in a corner, fade to 40% opacity, or collapse to a hairline strip against the screen edge.

## Features

- **Always on top**, frameless, with Windows 11 acrylic blur
- **Two note windows** max, distinguished by border colour (cyan / orange by default, changeable to any colour — the two can never be the same)
- **Live file watching** — edit the markdown in your editor or with Claude, and the viewer refreshes itself, preserving scroll position
- **Adjustable opacity** so it never fully blocks what's behind it
- **Dock to edge** — one click collapses a note to a 3.5×40 px strip; click the strip to restore
- **Native zoom** (`Ctrl` + scroll, 60–200%)
- **Light / dark theme**, following the system theme on first run
- **Drag from anywhere**, resize from any edge, capped at 80% of the current monitor
- Everything persists — size, position, opacity, zoom, theme, colours

## Install

### From a release (recommended)

1. Download `lite-notes-v1.0.0.zip` from the
   [Releases page](https://github.com/ajay47k/lite-notes-mcp/releases) and extract it
   somewhere permanent, e.g. `C:\Tools\lite-notes\`. It contains `lite-notes.exe` and
   `server.js`.
2. Register the MCP server with your Claude client. Keep `server.js` next to
   `lite-notes.exe` and the server finds the executable on its own.

   **Claude Desktop** — edit `%APPDATA%\Claude\claude_desktop_config.json`:
   ```json
   {
     "mcpServers": {
       "lite-notes": {
         "command": "node",
         "args": ["C:\\Tools\\lite-notes\\server.js"]
       }
     }
   }
   ```
   Restart Claude Desktop afterwards.

   **Claude Code**:
   ```powershell
   claude mcp add --scope user lite-notes -- node C:\Tools\lite-notes\server.js
   ```

   If you keep the executable somewhere else, point the `LITE_NOTES_EXE` environment
   variable at it (`"env": { "LITE_NOTES_EXE": "D:\\apps\\lite-notes.exe" }` in the
   Desktop config, or `--env LITE_NOTES_EXE=...` with `claude mcp add`).

> The executable is **not code-signed**, so the first launch shows a Windows SmartScreen
> prompt. Choose **More info → Run anyway**. Signing requires a paid certificate, which
> this project does not have.

### From source

Prerequisites: [Rust](https://rustup.rs) (MSVC toolchain), Visual Studio Build Tools with
the **Desktop development with C++** workload, and Node 18+ for the MCP server. WebView2
ships with Windows 11 — nothing to install there.

```powershell
git clone https://github.com/ajay47k/lite-notes-mcp.git
cd lite-notes-mcp\src-tauri
cargo build --release
# → src-tauri\target\release\lite-notes.exe
```

Then register the MCP server as above, pointing at `mcp\server.js` in the clone — it
resolves the freshly built executable automatically.

## Usage

Just ask, in plain language:

- *"show plan.md in lite-notes"* → opens it and watches it for changes
- *"put the spec in the second note"* → targets a specific window
- *"make the orange note's border green"* → changes a border colour
- *"close the notes"* → closes and cleans up

lite-notes never opens a window on its own; it only acts when you ask.

### MCP tools

| Tool | Purpose |
|---|---|
| `push_note(content \| path, slot?, title?)` | Display markdown. Pass `path` to live-watch a file on disk, or `content` for raw markdown that has no file (useful in Claude Desktop). Omit `slot` to auto-pick an empty one, else the least recently updated. |
| `close_note(slot)` | Close a note window and clean up its state. |
| `list_notes()` | Report both slots: state, mode, path, title, border colour. |
| `set_note_color(slot, color)` | Set a border colour from a hex value. Rejects a colour too close to the other window's. |

### Controls

| Action | How |
|---|---|
| Move | Drag any empty area |
| Resize | Drag any edge or corner |
| Zoom | `Ctrl` + scroll, or `Ctrl` + `+` / `-` / `0` |
| Opacity | Slider in the title bar |
| Theme | 🌓 button |
| Dock / restore | ☰ button, then click the strip |
| Close | ✕ button |

## How it works

```
Claude ──stdio──▶ mcp/server.js ──writes command json──▶ %LOCALAPPDATA%\lite-notes\commands\
                                                                    │ (file watcher)
                                                                    ▼
                                                          lite-notes.exe (Tauri)
                                                                    │
                                                          watches your .md file
```

The MCP server and the widget communicate only through files in
`%LOCALAPPDATA%\lite-notes\`. No sockets, no ports, no HTTP — the app works with
networking disabled entirely.

## Runtime data

`%LOCALAPPDATA%\lite-notes\` holds `commands\` (the MCP → widget inbox), `state.json`
(widget → MCP status), `settings.json` (your preferences), and `notes\` (temporary copies
for content-mode notes only).

Note content is ephemeral: closing a note or exiting the app deletes its temporary copy,
and a leftover copy from a crash is cleared on the next launch. Your preferences persist.

## Security

- The MCP server communicates over **stdio only** and opens no network connections.
- `push_note(path)` reads whatever file path Claude passes it, with the same permissions
  as your user account. Treat it like any other tool that can read your files, and review
  what you ask Claude to display.
- The widget is a **read-only viewer** — it never writes to a file you point it at.
- It **only ever deletes** its own temporary copies inside `%LOCALAPPDATA%\lite-notes\notes\`.
  A file you open with `push_note(path)` is never modified or deleted.

Rendering an untrusted markdown file is hardened against the obvious attacks:

| Threat | Mitigation |
|---|---|
| Script injection | Sanitised with DOMPurify before insertion |
| Tracking beacons / exfiltration via remote images | CSP `img-src 'self' asset: data:` — remote loads blocked. Local and embedded images still render |
| Form posting to an attacker | CSP `form-action 'none'` |
| A link hijacking the chromeless, always-on-top window | Navigation is restricted to the app's own origin in the Rust layer, with a click guard in the page as backup. External links are inert by design |
| UNC paths triggering outbound SMB auth (NTLM hash leak) | Network and device paths are rejected before reaching the widget |
| Oversized or non-regular files hanging the UI | Notes are capped at 5 MB and must be regular files |

## License

MIT — see [LICENSE](LICENSE). Bundled third-party components and their licenses are listed
in [NOTICE.md](NOTICE.md).
