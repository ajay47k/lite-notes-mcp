# lite-notes — Specification (v1.0)

## What it is
An always-on-top floating markdown viewer widget for Windows, integrated with Claude
Desktop and Claude Code through MCP, so markdown content can be read in a detached note
without consuming screen space.

## Stack & runtime
- **Tauri v2** — single executable, no browser UI, no server, fully offline
  (WebView2 is an OS component; the MCP server talks over stdio, not the network)
- Rendering: marked + DOMPurify + github-markdown-css + highlight.js, all vendored
  locally — no CDN requests at runtime

## Windows (max 2, one process)
| Item | Value |
|---|---|
| Default size | 320 × 240 logical px |
| Min / Max | 240 × 180 / **80% of the current monitor** — dynamic, multi-monitor and DPI aware |
| Resize | Native edge drag, persisted per slot |
| Drag | Full-body drag region, excluding controls and the scrollbar |
| Always on top | Enabled |
| Dock | Button collapses the window to a 3.5 × 40 px strip at the left edge; click the strip to restore |
| Zoom | Native WebView2 zoom (`Ctrl` + scroll, `Ctrl` + `+`/`-`/`0`), 60–200%, persisted per slot |
| Opacity | Slider — CSS translucency plus Windows 11 acrylic, persisted |
| Theme | Light / dark toggle, global across both windows, follows the system theme on first run, persisted |
| Borders | 2 px accent — slot 1 cyan `#22D3EE`, slot 2 orange `#F59E0B` by default; any RGB value accepted, but the two windows can **never** hold the same or a near-identical colour |

## MCP tools (4)
| Tool | Behaviour |
|---|---|
| `push_note(content \| path, slot?, title?)` | **Hybrid.** `path` live-watches a file on disk and auto-refreshes on every edit (preferred in Claude Code); `content` displays raw markdown that has no file, updated by pushing again (needed in Claude Desktop). With no `slot`, an empty one is chosen, otherwise the least recently updated. Launches the widget if it is not running. **Only ever invoked when the user explicitly asks — never proactively.** |
| `close_note(slot)` | Closes the window and clears its state |
| `list_notes()` | Per slot: state (open / docked / empty), mode, path, title, hex colour, last update |
| `set_note_color(slot, color)` | Sets a border colour from hex or `rgb()`. Rejects a value identical or too close to the other window's, returning the conflicting colour so a different shade can be chosen |

## Storage & cleanup
- `%LOCALAPPDATA%\lite-notes\` → `notes\slot-{1,2}.md` (content mode only),
  `commands\` (MCP → widget inbox), `state.json` (widget → MCP status), `settings.json`
- **Content mode:** the temporary copy is deleted when the note is closed or the app exits;
  a copy orphaned by a crash is removed on the next launch
- **Path mode:** closing only stops the watch — **the file on disk is never touched**
- Settings (size, position, opacity, zoom, theme, colours) persist; only note content is ephemeral
- A watched file that disappears shows a banner in the note and resumes if it returns;
  scroll position is preserved across refreshes

## IPC design (no network, no server)
```
Claude ──stdio──▶ MCP server (node mcp/server.js)
                    │  writes command json ──▶ %LOCALAPPDATA%\lite-notes\commands\
                    │  reads status        ◀── state.json (written by the widget)
                    │  spawns the executable if needed (single-instance guard dedupes)
                    ▼
              lite-notes.exe (Tauri) ── file watcher ──▶ command inbox + watched md files
```

## Setup
- Claude Desktop: `claude_desktop_config.json` →
  `{"mcpServers": {"lite-notes": {"command": "node", "args": ["<path>\\server.js"]}}}`
- Claude Code: `claude mcp add --scope user lite-notes -- node <path>\server.js`
- The same server works in both; a single-instance guard means they share the same two slots

## Deliberately out of scope (v1)
- No automatic pushing — Claude displays a note only when asked
- No colour palette UI — any RGB value is accepted and Claude resolves colour names itself
- No `get_note` / `append_note` — the widget is a read-only viewer for the current session
- No tray icon, pin toggle, or copy button
