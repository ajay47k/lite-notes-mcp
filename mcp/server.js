#!/usr/bin/env node
/**
 * lite-notes MCP server — stdio JSON-RPC, zero dependencies.
 * Talks to the widget via %LOCALAPPDATA%\lite-notes\ (commands inbox + state.json).
 * No network, no HTTP — pure files + stdio.
 */
"use strict";
const fs = require("fs");
const path = require("path");
const os = require("os");
const cp = require("child_process");
const readline = require("readline");

const DATA = path.join(
  process.env.LOCALAPPDATA || path.join(os.homedir(), "AppData", "Local"),
  "lite-notes"
);
const NOTES = path.join(DATA, "notes");
const CMDS = path.join(DATA, "commands");
const STATE = path.join(DATA, "state.json");
// exe lookup: explicit override, then release-zip layout (exe next to this
// file), then a source checkout's build output
const EXE = (() => {
  const candidates = [
    process.env.LITE_NOTES_EXE,
    path.join(__dirname, "lite-notes.exe"),
    path.join(__dirname, "..", "src-tauri", "target", "release", "lite-notes.exe"),
  ].filter(Boolean);
  return candidates.find((p) => fs.existsSync(p)) || candidates[0];
})();

const DEFAULT_COLORS = { 1: "#22D3EE", 2: "#F59E0B" };
const MIN_COLOR_DIST = 60;

for (const d of [DATA, NOTES, CMDS]) fs.mkdirSync(d, { recursive: true });

// ---------- helpers ----------
function readState() {
  try {
    return JSON.parse(fs.readFileSync(STATE, "utf8"));
  } catch (_) {
    return { slots: {}, colors: {} };
  }
}
function slotInfo(st, n) {
  return (st.slots && st.slots[String(n)]) || { state: "empty", last_updated: 0 };
}
function slotColor(st, n) {
  return (
    (st.colors && st.colors[String(n)]) ||
    (slotInfo(st, n).color || "").toUpperCase() ||
    DEFAULT_COLORS[n]
  );
}
function writeCmd(obj) {
  const name = `cmd-${Date.now()}-${Math.random().toString(36).slice(2, 8)}.json`;
  fs.writeFileSync(path.join(CMDS, name), JSON.stringify(obj));
}
function widgetRunning() {
  // real process check — a state.json freshness heuristic misfires while the
  // widget is idle and triggers duplicate spawns
  try {
    const out = cp.execSync('tasklist /FI "IMAGENAME eq lite-notes.exe" /NH', {
      encoding: "utf8", timeout: 3000, windowsHide: true,
    });
    return /lite-notes\.exe/i.test(out);
  } catch (_) { return false; }
}
function spawnWidget() {
  if (!fs.existsSync(EXE)) {
    return `widget exe not found at ${EXE} — build it first (cargo build --release)`;
  }
  if (widgetRunning()) return "already running";
  try {
    cp.spawn(EXE, [], { detached: true, stdio: "ignore", windowsHide: false }).unref();
  } catch (e) {
    return `failed to launch widget: ${e.message}`;
  }
  return null;
}
function parseColor(input) {
  if (typeof input !== "string") return null;
  let s = input.trim();
  let m = s.match(/^rgb\s*\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*\)$/i);
  if (m) {
    const [r, g, b] = [Number(m[1]), Number(m[2]), Number(m[3])];
    if ([r, g, b].some((v) => v > 255)) return null;
    return (
      "#" + [r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("").toUpperCase()
    );
  }
  m = s.match(/^#?([0-9a-f]{6})$/i);
  if (m) return "#" + m[1].toUpperCase();
  m = s.match(/^#?([0-9a-f]{3})$/i);
  if (m)
    return (
      "#" + m[1].split("").map((c) => c + c).join("").toUpperCase()
    );
  return null;
}
function colorDist(a, b) {
  const pa = parseColor(a), pb = parseColor(b);
  if (!pa || !pb) return Infinity;
  const v = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const [r1, g1, b1] = v(pa), [r2, g2, b2] = v(pb);
  return Math.sqrt((r1 - r2) ** 2 + (g1 - g2) ** 2 + (b1 - b2) ** 2);
}
function pickSlot(st, requested) {
  if (requested === 1 || requested === 2) return requested;
  const s1 = slotInfo(st, 1), s2 = slotInfo(st, 2);
  if (s1.state === "empty" || !s1.state) return 1;
  if (s2.state === "empty" || !s2.state) return 2;
  // both busy → least-recently-updated
  return (s1.last_updated || 0) <= (s2.last_updated || 0) ? 1 : 2;
}

// ---------- tool handlers ----------
const handlers = {
  push_note(args) {
    const { content, path: mdPath, slot: reqSlot, title } = args || {};
    if (content === undefined && !mdPath)
      return { error: "invalid_args", message: "Provide either 'content' (markdown text) or 'path' (md file to live-watch)." };
    if (content !== undefined && mdPath)
      return { error: "invalid_args", message: "Provide only one of 'content' or 'path', not both." };
    if (reqSlot !== undefined && reqSlot !== 1 && reqSlot !== 2)
      return { error: "invalid_slot", message: "slot must be 1 or 2" };
    // validate launchability BEFORE committing any side effects
    if (!fs.existsSync(EXE))
      return { error: "launch_failed", message: `widget exe not found at ${EXE} — build it first (cargo build --release)` };

    let mode, absPath = "";
    if (mdPath) {
      mode = "path";
      absPath = path.resolve(mdPath);
      if (!fs.existsSync(absPath))
        return { error: "file_not_found", path: absPath };
    } else {
      mode = "content";
    }

    const st = readState();
    const slot = pickSlot(st, reqSlot);
    const replaced = slotInfo(st, slot).state && slotInfo(st, slot).state !== "empty";

    // content travels inside the command json — the widget writes the slot
    // file itself, so no note file can exist without its command (no orphan
    // race with the widget's startup cleanup)
    writeCmd({
      cmd: "push", slot, mode, path: absPath, title: title || "",
      content: mode === "content" ? String(content) : undefined,
    });
    const launchMsg = spawnWidget();
    if (launchMsg && launchMsg !== "already running") return { error: "launch_failed", message: launchMsg };

    return {
      slot,
      color: slotColor(st, slot),
      mode,
      replaced: !!replaced,
      watching: mode === "path" ? absPath : undefined,
    };
  },

  close_note(args) {
    const slot = args && args.slot;
    if (slot !== 1 && slot !== 2)
      return { error: "invalid_slot", message: "slot must be 1 or 2" };
    writeCmd({ cmd: "close", slot });
    return { closed: true, slot };
  },

  list_notes() {
    const st = readState();
    return {
      notes: [1, 2].map((n) => {
        const s = slotInfo(st, n);
        return {
          slot: n,
          state: s.state || "empty",
          mode: s.mode || "",
          path: s.path || "",
          title: s.title || "",
          color: slotColor(st, n),
          last_updated: s.last_updated || 0,
        };
      }),
      theme: st.theme || "dark",
    };
  },

  set_note_color(args) {
    const { slot, color } = args || {};
    if (slot !== 1 && slot !== 2)
      return { error: "invalid_slot", message: "slot must be 1 or 2" };
    const hex = parseColor(color);
    if (!hex)
      return { error: "invalid_color", message: "Pass hex like #1A2B3C or rgb(r,g,b) with 0-255 channels." };
    const st = readState();
    const other = slot === 1 ? 2 : 1;
    const otherColor = slotColor(st, other);
    const dist = colorDist(hex, otherColor);
    if (dist === 0)
      return { error: "color_taken", other_window: otherColor, message: `Window ${other} already uses this color. Pick a visibly different one.` };
    if (dist < MIN_COLOR_DIST)
      return { error: "too_similar", other_window: otherColor, message: `Too close to window ${other}'s color ${otherColor}. Pick a visibly different one.` };
    writeCmd({ cmd: "set_color", slot, color: hex });
    return { slot, color: hex };
  },
};

// ---------- MCP tool schemas ----------
const TOOLS = [
  {
    name: "push_note",
    description:
      "Display markdown in the lite-notes floating always-on-top widget (detached note window). " +
      "Use ONLY when the user explicitly asks to show/launch/display a note or md file in lite-notes — never proactively. " +
      "Two modes: pass 'path' to live-watch an md file on disk (edits auto-refresh in the viewer — preferred in Claude Code), " +
      "or pass 'content' with raw markdown when no file exists (Claude Desktop; update by pushing again to the same slot).",
    inputSchema: {
      type: "object",
      properties: {
        content: { type: "string", description: "Raw markdown to display (mutually exclusive with path)" },
        path: { type: "string", description: "Absolute path of an .md file to display and live-watch (mutually exclusive with content)" },
        slot: { type: "number", enum: [1, 2], description: "Target window: 1 or 2. Omit to auto-pick (empty slot, else least-recently-updated)" },
        title: { type: "string", description: "Optional title shown in the note header (defaults to filename)" },
      },
    },
  },
  {
    name: "close_note",
    description: "Close a lite-notes window. Content-mode note file is deleted; a live-watched file on disk is never touched.",
    inputSchema: {
      type: "object",
      properties: { slot: { type: "number", enum: [1, 2] } },
      required: ["slot"],
    },
  },
  {
    name: "list_notes",
    description: "Status of both lite-notes slots: state (open/docked/empty), mode, path, title, border color hex, last update time.",
    inputSchema: { type: "object", properties: {} },
  },
  {
    name: "set_note_color",
    description:
      "Set a note window's border color. Convert the user's requested color name to a hex value yourself and pass it. " +
      "The two windows can never have the same or nearly-identical color — on error 'color_taken'/'too_similar' pick a visibly different shade and retry.",
    inputSchema: {
      type: "object",
      properties: {
        slot: { type: "number", enum: [1, 2] },
        color: { type: "string", description: "Hex #RRGGBB (or rgb(r,g,b), channels 0-255)" },
      },
      required: ["slot", "color"],
    },
  },
];

// ---------- JSON-RPC over stdio (newline-delimited) ----------
function send(msg) {
  process.stdout.write(JSON.stringify(msg) + "\n");
}
const rl = readline.createInterface({ input: process.stdin, terminal: false });
rl.on("line", (line) => {
  line = line.trim();
  if (!line) return;
  let req;
  try {
    req = JSON.parse(line);
  } catch (_) {
    send({ jsonrpc: "2.0", id: null, error: { code: -32700, message: "parse error" } });
    return;
  }
  const { id, method, params } = req;
  const reply = (result) => id !== undefined && send({ jsonrpc: "2.0", id, result });
  const fail = (code, message) => id !== undefined && send({ jsonrpc: "2.0", id, error: { code, message } });

  try {
    switch (method) {
      case "initialize":
        reply({
          protocolVersion: (params && params.protocolVersion) || "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "lite-notes", version: "1.0.0" },
        });
        break;
      case "notifications/initialized":
      case "notifications/cancelled":
        break; // notifications: no response
      case "ping":
        reply({});
        break;
      case "tools/list":
        reply({ tools: TOOLS });
        break;
      case "tools/call": {
        const name = params && params.name;
        const args = (params && params.arguments) || {};
        const h = handlers[name];
        if (!h) return fail(-32602, `unknown tool: ${name}`);
        const result = h(args);
        reply({
          content: [{ type: "text", text: JSON.stringify(result, null, 2) }],
          isError: !!(result && result.error),
        });
        break;
      }
      default:
        if (id !== undefined) fail(-32601, `method not found: ${method}`);
    }
  } catch (e) {
    fail(-32603, e.message || "internal error");
  }
});
rl.on("close", () => process.exit(0));
