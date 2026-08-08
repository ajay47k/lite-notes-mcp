const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const slot = Number(new URLSearchParams(location.search).get("slot") || "1");
const $ = (id) => document.getElementById(id);
const content = $("content");

let zoom = 1.0;
let docked = false;

marked.setOptions({ gfm: true, breaks: false });

function render(md, missing) {
  $("banner").hidden = !missing;
  if (missing) return; // keep last rendered content while file is gone
  const st = content.scrollTop;
  const html = DOMPurify.sanitize(marked.parse(md || ""));
  content.innerHTML = html;
  content.querySelectorAll("pre code").forEach((el) => {
    try { hljs.highlightElement(el); } catch (_) {}
  });
  content.scrollTop = st; // preserve scroll on refresh
}

// Defense in depth alongside the Rust-side navigation guard: a link inside an
// untrusted note must not be able to steer this chromeless window anywhere.
content.addEventListener("click", (e) => {
  const a = e.target.closest && e.target.closest("a");
  if (a) e.preventDefault();
});

function applyColor(color) {
  document.documentElement.style.setProperty("--accent", color);
}

function applyTheme(theme) {
  document.body.dataset.theme = theme;
  const light = theme === "light";
  $("md-light").disabled = !light;
  $("md-dark").disabled = light;
  $("hl-light").disabled = !light;
  $("hl-dark").disabled = light;
}

function applyOpacity(v) {
  document.documentElement.style.setProperty("--bg-alpha", v);
  $("opacity").value = Math.round(v * 100);
}

function setDocked(on) {
  docked = on;
  document.body.classList.toggle("docked", on);
  $("strip").hidden = !on;
}

async function init() {
  const d = await invoke("frontend_ready", { slot });
  applyColor(d.color);
  applyTheme(d.theme);
  applyOpacity(d.opacity);
  zoom = d.zoom || 1.0;
  $("title").textContent = d.title || "lite-notes";
  setDocked(d.state === "docked");
  render(d.content, d.missing);
}

// ---- events from Rust ----
listen("note-updated", (e) => {
  if (e.payload.slot !== slot) return;
  $("title").textContent = e.payload.title || "lite-notes";
  applyColor(e.payload.color);
  render(e.payload.content, e.payload.missing);
});
listen("settings-changed", (e) => {
  if (e.payload.opacity !== undefined) applyOpacity(e.payload.opacity);
  if (e.payload.theme !== undefined) applyTheme(e.payload.theme);
});
listen("color-changed", (e) => {
  if (e.payload.slot === slot) applyColor(e.payload.color);
});
listen("dock-changed", (e) => {
  if (e.payload.slot === slot) setDocked(e.payload.docked);
});

// ---- controls ----
$("opacity").addEventListener("input", (e) => {
  const v = Number(e.target.value) / 100;
  document.documentElement.style.setProperty("--bg-alpha", v);
});
$("opacity").addEventListener("change", (e) => {
  invoke("set_opacity", { value: Number(e.target.value) / 100 });
});
$("btn-theme").addEventListener("click", () => {
  const next = document.body.dataset.theme === "dark" ? "light" : "dark";
  invoke("set_theme", { theme: next });
});
$("btn-dock").addEventListener("click", () => {
  setDocked(true);
  invoke("dock_slot", { slot });
});
$("strip").addEventListener("click", () => {
  setDocked(false);
  invoke("undock_slot", { slot });
});
$("btn-close").addEventListener("click", () => invoke("close_slot", { slot }));

// ---- drag on empty content area (children = text stays selectable; the
// scrollbar strip on the right must scroll, not move the window) ----
content.addEventListener("mousedown", (e) => {
  if (e.button !== 0 || e.target !== content) return;
  if (e.offsetX >= content.clientWidth) return; // scrollbar hit-zone
  window.__TAURI__.window.getCurrentWindow().startDragging();
});

// ---- zoom: Ctrl+scroll / Ctrl +/-/0, native WebView2 set_zoom under the hood ----
function setZoom(f) {
  zoom = Math.min(2.0, Math.max(0.6, Math.round(f * 10) / 10));
  invoke("set_zoom", { slot, factor: zoom });
}
window.addEventListener(
  "wheel",
  (e) => {
    if (!e.ctrlKey) return;
    e.preventDefault();
    setZoom(zoom + (e.deltaY < 0 ? 0.1 : -0.1));
  },
  { passive: false }
);
window.addEventListener("keydown", (e) => {
  if (!e.ctrlKey) return;
  if (e.key === "=" || e.key === "+") { e.preventDefault(); setZoom(zoom + 0.1); }
  else if (e.key === "-") { e.preventDefault(); setZoom(zoom - 0.1); }
  else if (e.key === "0") { e.preventDefault(); setZoom(1.0); }
});

init();
