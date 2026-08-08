#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, PhysicalPosition, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

const DEFAULT_COLORS: [&str; 2] = ["#22D3EE", "#F59E0B"]; // slot 1 cyan, slot 2 orange
const MIN_COLOR_DIST: f64 = 60.0;
const MAX_NOTE_BYTES: u64 = 5 * 1024 * 1024;
const WELCOME_MD: &str = "# lite-notes\n\nAbhi koi note push nahi hua.\n\nClaude se bolo:\n\n> *\"is md file ko lite-notes me dikhao\"*\n\n- Drag: khali jagah pakad ke kheencho\n- `Ctrl` + scroll: zoom\n- ☰ button: dock to edge\n";

#[derive(Serialize, Deserialize, Clone, Default)]
struct SlotInfo {
    state: String, // "empty" | "open" | "docked"
    mode: String,  // "content" | "path" | ""
    path: String,
    title: String,
    color: String,
    last_updated: u64,
}

#[derive(Serialize, Deserialize, Clone)]
struct Settings {
    theme: String,
    opacity: f64,
    zoom: HashMap<String, f64>,
    colors: HashMap<String, String>,
    bounds: HashMap<String, [f64; 4]>, // logical x,y,w,h per slot
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: String::new(), // resolved to system theme on first run
            opacity: 0.92,
            zoom: HashMap::new(),
            colors: HashMap::new(),
            bounds: HashMap::new(),
        }
    }
}

struct AppData {
    slots: [SlotInfo; 2],
    settings: Settings,
    prev_bounds: HashMap<u8, [f64; 4]>, // pre-dock bounds for restore
    closing: [bool; 2],                 // window close in flight — don't reuse the label
    last_settings_save: Instant,
}

// watcher lives behind its own lock: never hold the state lock while touching
// notify, and never touch notify from its own callback (ABBA deadlock)
struct WatchState {
    watcher: Option<RecommendedWatcher>,
    dirs: Vec<PathBuf>,
}

type State = Arc<Mutex<AppData>>;
type Watch = Arc<Mutex<WatchState>>;

// poison-proof: a panic anywhere must never brick every later lock
fn lockd<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn data_dir() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("lite-notes")
}
fn notes_dir() -> PathBuf {
    data_dir().join("notes")
}
fn commands_dir() -> PathBuf {
    data_dir().join("commands")
}
fn state_file() -> PathBuf {
    data_dir().join("state.json")
}
fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// case-insensitive canonical form for path comparison (Windows)
fn canon(p: &Path) -> String {
    fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .to_lowercase()
}

fn atomic_write(path: &Path, content: &str) {
    let tmp = path.with_extension("tmp");
    if fs::write(&tmp, content).is_ok() {
        let _ = fs::rename(&tmp, path); // MoveFileEx REPLACE_EXISTING on Windows
    }
}

fn hex_to_rgb(hex: &str) -> Option<(f64, f64, f64)> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()? as f64;
    let g = u8::from_str_radix(&h[2..4], 16).ok()? as f64;
    let b = u8::from_str_radix(&h[4..6], 16).ok()? as f64;
    Some((r, g, b))
}

fn color_dist(a: &str, b: &str) -> f64 {
    match (hex_to_rgb(a), hex_to_rgb(b)) {
        (Some(x), Some(y)) => {
            ((x.0 - y.0).powi(2) + (x.1 - y.1).powi(2) + (x.2 - y.2).powi(2)).sqrt()
        }
        _ => f64::MAX,
    }
}

fn slot_color(data: &AppData, slot: u8) -> String {
    data.settings
        .colors
        .get(&slot.to_string())
        .cloned()
        .unwrap_or_else(|| DEFAULT_COLORS[(slot - 1) as usize].to_string())
}

fn load_settings() -> Settings {
    fs::read_to_string(settings_file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(data: &AppData) {
    atomic_write(
        &settings_file(),
        &serde_json::to_string_pretty(&data.settings).unwrap_or_default(),
    );
}

fn write_state(data: &AppData) {
    let slots: HashMap<String, &SlotInfo> = data
        .slots
        .iter()
        .enumerate()
        .map(|(i, s)| ((i + 1).to_string(), s))
        .collect();
    let colors: HashMap<String, String> = (1u8..=2)
        .map(|n| (n.to_string(), slot_color(data, n)))
        .collect();
    let theme = if data.settings.theme.is_empty() {
        "dark"
    } else {
        &data.settings.theme
    };
    let st = json!({
        "slots": slots,
        "colors": colors,
        "theme": theme,
        "opacity": data.settings.opacity,
        "updated": now_ts(),
    });
    atomic_write(
        &state_file(),
        &serde_json::to_string_pretty(&st).unwrap_or_default(),
    );
}

fn read_note_content(slot: &SlotInfo) -> (String, bool) {
    if slot.path.is_empty() {
        return (WELCOME_MD.to_string(), false);
    }
    // Guard before reading: a directory or device path would block forever and
    // an oversized file would exhaust memory on the UI thread. Commands can
    // arrive straight from the inbox, so this cannot rely on the MCP server's
    // own validation.
    match fs::metadata(&slot.path) {
        Ok(m) if !m.is_file() => return (String::new(), true),
        Ok(m) if m.len() > MAX_NOTE_BYTES => {
            return (
                format!(
                    "> **File too large to display** — {:.1} MB, limit is {} MB.",
                    m.len() as f64 / 1_048_576.0,
                    MAX_NOTE_BYTES / 1_048_576
                ),
                false,
            )
        }
        Ok(_) => {}
        Err(_) => return (String::new(), true),
    }
    match fs::read_to_string(&slot.path) {
        Ok(c) => (c, false),
        Err(_) => (String::new(), true),
    }
}

fn window_label(slot: u8) -> String {
    format!("note-{}", slot)
}

fn valid_slot(slot: u8) -> bool {
    (1..=2).contains(&slot)
}

fn apply_theme_vibrancy(win: &WebviewWindow, theme: &str) {
    let tint = if theme == "light" {
        (245u8, 245u8, 245u8, 160u8)
    } else {
        (16u8, 16u8, 16u8, 160u8)
    };
    let _ = window_vibrancy::apply_acrylic(win, Some(tint));
}

fn apply_max_size(win: &WebviewWindow) {
    if let Ok(Some(mon)) = win.current_monitor() {
        let scale = mon.scale_factor();
        let w = mon.size().width as f64 / scale * 0.8;
        let h = mon.size().height as f64 / scale * 0.8;
        let _ = win.set_max_size(Some(LogicalSize::new(w, h)));
    }
}

fn resolve_theme(app: &AppHandle, saved: &str) -> String {
    if saved == "light" || saved == "dark" {
        return saved.to_string();
    }
    // first run: follow system theme
    let sys_light = (1u8..=2)
        .filter_map(|n| app.get_webview_window(&window_label(n)))
        .next()
        .and_then(|w| w.theme().ok())
        .map(|t| t == tauri::Theme::Light)
        .unwrap_or(false);
    if sys_light {
        "light".into()
    } else {
        "dark".into()
    }
}

/// Must only be called on the main thread (window creation is not
/// thread-safe on Windows — off-main creation produces half-initialized
/// "ghost" windows).
fn ensure_window(app: &AppHandle, slot: u8) -> Option<WebviewWindow> {
    let label = window_label(slot);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_focus();
        return Some(w);
    }
    let state: tauri::State<State> = app.state();
    let (bounds, zoom, theme) = {
        let data = lockd(&state);
        (
            data.settings.bounds.get(&slot.to_string()).copied(),
            data.settings
                .zoom
                .get(&slot.to_string())
                .copied()
                .unwrap_or(1.0),
            data.settings.theme.clone(),
        )
    };
    let (w, h) = bounds.map(|b| (b[2], b[3])).unwrap_or((320.0, 240.0));
    let win = WebviewWindowBuilder::new(
        app,
        &label,
        WebviewUrl::App(format!("index.html?slot={}", slot).into()),
    )
    .title(format!("lite-notes {}", slot))
    .inner_size(w, h)
    .min_inner_size(240.0, 180.0)
    .resizable(true)
    .decorations(false)
    .transparent(true)
    .always_on_top(true)
    .skip_taskbar(true)
    .visible(true)
    // an untrusted note must never be able to navigate this chromeless,
    // always-on-top window somewhere else — there is no URL bar or back
    // button to escape with. Only the app's own origin may load.
    .on_navigation(|url| {
        let host = url.host_str().unwrap_or("");
        host.is_empty() || host == "localhost" || host.ends_with(".localhost")
    })
    .build()
    .ok()?;

    // position: saved, else top-right stack per slot
    if let Some(b) = bounds {
        let scale = win.scale_factor().unwrap_or(1.0);
        let _ = win.set_position(PhysicalPosition::new(
            (b[0] * scale) as i32,
            (b[1] * scale) as i32,
        ));
    } else if let Ok(Some(mon)) = win.current_monitor() {
        let scale = mon.scale_factor();
        let x = mon.position().x as f64 + mon.size().width as f64 - (w + 24.0) * scale;
        let y = mon.position().y as f64 + (56.0 + (slot as f64 - 1.0) * 272.0) * scale;
        let _ = win.set_position(PhysicalPosition::new(x as i32, y as i32));
    }
    apply_max_size(&win);
    let theme = if theme.is_empty() { "dark".to_string() } else { theme };
    apply_theme_vibrancy(&win, &theme);
    let _ = win.set_zoom(zoom);
    Some(win)
}

/// (Re)compute the set of directories the watcher should cover and
/// add/remove watches to match. Never called while holding the state lock's
/// guard across the notify calls.
fn sync_watches(app: &AppHandle) {
    let state: tauri::State<State> = app.state();
    let mut needed: Vec<PathBuf> = vec![commands_dir(), notes_dir()];
    {
        let data = lockd(&state);
        for info in data.slots.iter() {
            if !info.path.is_empty() {
                if let Some(parent) = Path::new(&info.path).parent() {
                    let p = parent.to_path_buf();
                    if !needed.iter().any(|d| canon(d) == canon(&p)) {
                        needed.push(p);
                    }
                }
            }
        }
    }
    let watch: tauri::State<Watch> = app.state();
    let mut ws = lockd(&watch);
    let current = ws.dirs.clone();
    if let Some(w) = ws.watcher.as_mut() {
        for d in &current {
            if !needed.iter().any(|n| canon(n) == canon(d)) {
                let _ = w.unwatch(d);
            }
        }
        for n in &needed {
            if !current.iter().any(|d| canon(d) == canon(n)) {
                let _ = w.watch(n, RecursiveMode::NonRecursive);
            }
        }
    }
    ws.dirs = needed;
}

fn emit_note(app: &AppHandle, slot: u8) {
    if !valid_slot(slot) {
        return;
    }
    let state: tauri::State<State> = app.state();
    let (info, color) = {
        let data = lockd(&state);
        (data.slots[(slot - 1) as usize].clone(), slot_color(&data, slot))
    };
    let (content, missing) = read_note_content(&info);
    let _ = app.emit(
        "note-updated",
        json!({
            "slot": slot,
            "content": content,
            "missing": missing,
            "title": info.title,
            "color": color,
        }),
    );
}

fn cleanup_slot(app: &AppHandle, slot: u8, close_window: bool) {
    if !valid_slot(slot) {
        return;
    }
    let state: tauri::State<State> = app.state();
    {
        let mut data = lockd(&state);
        let color = slot_color(&data, slot);
        let info = &mut data.slots[(slot - 1) as usize];
        if info.mode == "content" && !info.path.is_empty() {
            let _ = fs::remove_file(&info.path);
        }
        *info = SlotInfo {
            state: "empty".into(),
            color,
            ..Default::default()
        };
        if close_window {
            data.closing[(slot - 1) as usize] = true;
        }
        write_state(&data);
    }
    sync_watches(app);
    if close_window {
        if let Some(w) = app.get_webview_window(&window_label(slot)) {
            let _ = w.close();
        } else {
            lockd(&state).closing[(slot - 1) as usize] = false;
        }
    }
}

fn restore_dock_geometry(app: &AppHandle, slot: u8) {
    let win = match app.get_webview_window(&window_label(slot)) {
        Some(w) => w,
        None => return,
    };
    let state: tauri::State<State> = app.state();
    let b = lockd(&state)
        .prev_bounds
        .get(&slot)
        .copied()
        .unwrap_or([100.0, 100.0, 320.0, 240.0]);
    let _ = win.set_min_size(Some(LogicalSize::new(240.0, 180.0)));
    let _ = win.set_resizable(true);
    let _ = win.set_size(LogicalSize::new(b[2], b[3]));
    let scale = win.scale_factor().unwrap_or(1.0);
    let _ = win.set_position(PhysicalPosition::new(
        (b[0] * scale) as i32,
        (b[1] * scale) as i32,
    ));
}

fn handle_push(app: &AppHandle, cmd: &serde_json::Value, retries: u8) {
    let slot = cmd["slot"].as_u64().unwrap_or(1) as u8;
    if !valid_slot(slot) {
        return;
    }
    // label of a closing window can't be reused yet — retry after Destroyed
    let state: tauri::State<State> = app.state();
    if lockd(&state).closing[(slot - 1) as usize] {
        if retries < 15 {
            let h = app.clone();
            let c = cmd.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                let h2 = h.clone();
                let _ = h.run_on_main_thread(move || handle_push(&h2, &c, retries + 1));
            });
        }
        return;
    }
    let mode = cmd["mode"].as_str().unwrap_or("content").to_string();
    let path = if mode == "path" {
        let p = cmd["path"].as_str().unwrap_or("").to_string();
        // store canonical form so watcher events compare reliably
        // (strip the \\?\ verbatim prefix for readable state.json output)
        fs::canonicalize(&p)
            .map(|c| {
                c.to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .to_string()
            })
            .unwrap_or(p)
    } else {
        // content travels inside the command — the widget owns the slot file,
        // so a note file can never exist without its command (no orphan race)
        let file = notes_dir().join(format!("slot-{}.md", slot));
        let _ = fs::write(&file, cmd["content"].as_str().unwrap_or(""));
        file.to_string_lossy().to_string()
    };
    let title = {
        let t = cmd["title"].as_str().unwrap_or("");
        if t.is_empty() {
            Path::new(&path)
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_else(|| format!("note {}", slot))
        } else {
            t.to_string()
        }
    };
    let was_docked = {
        let mut data = lockd(&state);
        let was_docked = data.slots[(slot - 1) as usize].state == "docked";
        let color = slot_color(&data, slot);
        data.slots[(slot - 1) as usize] = SlotInfo {
            state: "open".into(),
            mode,
            path: path.clone(),
            title,
            color,
            last_updated: now_ts(),
        };
        write_state(&data);
        was_docked
    };
    sync_watches(app);
    ensure_window(app, slot);
    if was_docked {
        restore_dock_geometry(app, slot);
        let _ = app.emit("dock-changed", json!({ "slot": slot, "docked": false }));
    }
    emit_note(app, slot);
}

fn handle_set_color(app: &AppHandle, cmd: &serde_json::Value) {
    let slot = cmd["slot"].as_u64().unwrap_or(0) as u8;
    let color = cmd["color"].as_str().unwrap_or("").to_uppercase();
    if !valid_slot(slot) || hex_to_rgb(&color).is_none() {
        return;
    }
    let other = if slot == 1 { 2 } else { 1 };
    let state: tauri::State<State> = app.state();
    {
        let mut data = lockd(&state);
        let other_color = slot_color(&data, other);
        if color_dist(&color, &other_color) < MIN_COLOR_DIST {
            return; // uniqueness rule — MCP side already reports the error
        }
        data.settings.colors.insert(slot.to_string(), color.clone());
        data.slots[(slot - 1) as usize].color = color.clone();
        save_settings(&data);
        write_state(&data);
    }
    let _ = app.emit("color-changed", json!({ "slot": slot, "color": color }));
}

/// Must only run on the main thread — commands create/close windows.
fn process_inbox(app: &AppHandle) {
    let dir = commands_dir();
    let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    for file in entries {
        let cmd: serde_json::Value = match fs::read_to_string(&file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(c) => c,
            None => {
                let _ = fs::remove_file(&file);
                continue;
            }
        };
        let _ = fs::remove_file(&file);
        match cmd["cmd"].as_str().unwrap_or("") {
            "push" => handle_push(app, &cmd, 0),
            "close" => {
                let slot = cmd["slot"].as_u64().unwrap_or(0) as u8;
                cleanup_slot(app, slot, true);
            }
            "set_color" => handle_set_color(app, &cmd),
            _ => {}
        }
    }
}

fn schedule_inbox(app: &AppHandle) {
    let h = app.clone();
    let _ = app.run_on_main_thread(move || {
        let h2 = h.clone();
        process_inbox(&h2);
    });
}

fn startup_cleanup() {
    // content notes always arrive inside push commands now, so every file in
    // notes\ at boot is an orphan from a crashed session
    if let Ok(rd) = fs::read_dir(notes_dir()) {
        for e in rd.filter_map(|e| e.ok()) {
            let _ = fs::remove_file(e.path());
        }
    }
}

fn final_cleanup(state: &State) {
    let mut data = lockd(state);
    for i in 0..2 {
        let info = &mut data.slots[i];
        if info.mode == "content" && !info.path.is_empty() {
            let _ = fs::remove_file(&info.path);
        }
        info.state = "empty".into();
        info.mode = String::new();
        info.path = String::new();
        info.title = String::new();
    }
    write_state(&data);
    save_settings(&data);
}

// ---------- invoke commands (sync commands run on the main thread) ----------

#[tauri::command]
fn frontend_ready(app: AppHandle, slot: u8) -> serde_json::Value {
    if !valid_slot(slot) {
        return json!({ "error": "invalid slot" });
    }
    let state: tauri::State<State> = app.state();
    let theme = {
        let saved = lockd(&state).settings.theme.clone();
        let resolved = resolve_theme(&app, &saved);
        let mut data = lockd(&state);
        if data.settings.theme != resolved {
            data.settings.theme = resolved.clone();
            save_settings(&data);
            write_state(&data);
        }
        resolved
    };
    if let Some(w) = app.get_webview_window(&window_label(slot)) {
        apply_theme_vibrancy(&w, &theme);
    }
    let data = lockd(&state);
    let info = &data.slots[(slot - 1) as usize];
    let (content, missing) = read_note_content(info);
    json!({
        "slot": slot,
        "content": content,
        "missing": missing,
        "title": info.title,
        "state": info.state,
        "color": slot_color(&data, slot),
        "theme": theme,
        "opacity": data.settings.opacity,
        "zoom": data.settings.zoom.get(&slot.to_string()).copied().unwrap_or(1.0),
    })
}

#[tauri::command]
fn set_opacity(app: AppHandle, value: f64) {
    let v = value.clamp(0.3, 1.0);
    let state: tauri::State<State> = app.state();
    {
        let mut data = lockd(&state);
        data.settings.opacity = v;
        save_settings(&data);
        write_state(&data);
    }
    let _ = app.emit("settings-changed", json!({ "opacity": v }));
}

#[tauri::command]
fn set_theme(app: AppHandle, theme: String) {
    let t = if theme == "light" { "light" } else { "dark" };
    let state: tauri::State<State> = app.state();
    {
        let mut data = lockd(&state);
        data.settings.theme = t.to_string();
        save_settings(&data);
        write_state(&data);
    }
    for slot in 1u8..=2 {
        if let Some(w) = app.get_webview_window(&window_label(slot)) {
            apply_theme_vibrancy(&w, t);
        }
    }
    let _ = app.emit("settings-changed", json!({ "theme": t }));
}

#[tauri::command]
fn set_zoom(app: AppHandle, slot: u8, factor: f64) {
    if !valid_slot(slot) {
        return;
    }
    let f = factor.clamp(0.6, 2.0);
    if let Some(w) = app.get_webview_window(&window_label(slot)) {
        let _ = w.set_zoom(f);
    }
    let state: tauri::State<State> = app.state();
    let mut data = lockd(&state);
    data.settings.zoom.insert(slot.to_string(), f);
    // wheel ticks come in bursts — throttle disk writes; exit flush covers the tail
    if data.last_settings_save.elapsed() > Duration::from_millis(300) {
        data.last_settings_save = Instant::now();
        save_settings(&data);
    }
}

#[tauri::command]
fn dock_slot(app: AppHandle, slot: u8) {
    if !valid_slot(slot) {
        return;
    }
    let win = match app.get_webview_window(&window_label(slot)) {
        Some(w) => w,
        None => return,
    };
    let state: tauri::State<State> = app.state();
    let scale = win.scale_factor().unwrap_or(1.0);
    // mark docked BEFORE resizing — WM_SIZE can dispatch synchronously and the
    // Moved/Resized handler must not persist the 14x160 strip as real bounds
    {
        let mut data = lockd(&state);
        if let (Ok(pos), Ok(size)) = (win.outer_position(), win.inner_size()) {
            data.prev_bounds.insert(
                slot,
                [
                    pos.x as f64 / scale,
                    pos.y as f64 / scale,
                    size.width as f64 / scale,
                    size.height as f64 / scale,
                ],
            );
        }
        data.slots[(slot - 1) as usize].state = "docked".into();
        write_state(&data);
    }
    let _ = win.set_resizable(false);
    let _ = win.set_min_size(None::<LogicalSize<f64>>); // lift 240x180 so the strip can shrink
    let _ = win.set_size(LogicalSize::new(3.5, 40.0));
    if let (Ok(Some(mon)), Ok(pos)) = (win.current_monitor(), win.outer_position()) {
        let _ = win.set_position(PhysicalPosition::new(mon.position().x, pos.y));
    }
    let _ = app.emit("dock-changed", json!({ "slot": slot, "docked": true }));
}

#[tauri::command]
fn undock_slot(app: AppHandle, slot: u8) {
    if !valid_slot(slot) {
        return;
    }
    restore_dock_geometry(&app, slot);
    let state: tauri::State<State> = app.state();
    {
        let mut data = lockd(&state);
        let i = (slot - 1) as usize;
        if data.slots[i].state == "docked" {
            data.slots[i].state = "open".into();
        }
        write_state(&data);
    }
    let _ = app.emit("dock-changed", json!({ "slot": slot, "docked": false }));
}

#[tauri::command]
fn close_slot(app: AppHandle, slot: u8) {
    cleanup_slot(&app, slot, true);
}

// ---------- main ----------

/// Single-instance guard via named mutex, taken as the FIRST thing in main.
/// The tauri single-instance plugin registers its mutex only after full
/// builder init (~hundreds of ms) — two MCP pushes spawning the exe
/// back-to-back both got through that window, leaving two processes each
/// owning one window ("ghost" windows a close command can never reach).
/// The OS releases the mutex automatically when the process dies.
fn acquire_single_instance() -> bool {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;
    let name: Vec<u16> = "Local\\lite-notes-singleton\0".encode_utf16().collect();
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        !(h.is_null() || GetLastError() == ERROR_ALREADY_EXISTS)
    }
}

fn main() {
    if !acquire_single_instance() {
        // primary instance's file watcher will pick up any pending commands
        return;
    }
    let state: State = Arc::new(Mutex::new(AppData {
        slots: [SlotInfo::default(), SlotInfo::default()],
        settings: Settings::default(),
        prev_bounds: HashMap::new(),
        closing: [false, false],
        last_settings_save: Instant::now(),
    }));
    let watch: Watch = Arc::new(Mutex::new(WatchState {
        watcher: None,
        dirs: Vec::new(),
    }));

    let app = tauri::Builder::default()
        .manage(state.clone())
        .manage(watch.clone())
        .invoke_handler(tauri::generate_handler![
            frontend_ready,
            set_opacity,
            set_theme,
            set_zoom,
            dock_slot,
            undock_slot,
            close_slot
        ])
        .on_window_event(|window, event| {
            let app = window.app_handle();
            let label = window.label().to_string();
            let slot: u8 = match label.strip_prefix("note-").and_then(|s| s.parse().ok()) {
                Some(s) if valid_slot(s) => s,
                _ => return,
            };
            match event {
                tauri::WindowEvent::CloseRequested { .. } => {
                    // Alt+F4 path; after close_slot this is a no-op (slot already empty)
                    cleanup_slot(app, slot, false);
                    let state: tauri::State<State> = app.state();
                    lockd(&state).closing[(slot - 1) as usize] = true;
                }
                tauri::WindowEvent::Destroyed => {
                    let state: tauri::State<State> = app.state();
                    lockd(&state).closing[(slot - 1) as usize] = false;
                }
                tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Resized(_) => {
                    let state: tauri::State<State> = app.state();
                    let skip = {
                        let data = lockd(&state);
                        data.slots[(slot - 1) as usize].state == "docked"
                            || data.closing[(slot - 1) as usize]
                    };
                    if skip {
                        return;
                    }
                    if let Some(w) = app.get_webview_window(&label) {
                        apply_max_size(&w);
                        let scale = w.scale_factor().unwrap_or(1.0);
                        if let (Ok(pos), Ok(size)) = (w.outer_position(), w.inner_size()) {
                            let mut data = lockd(&state);
                            data.settings.bounds.insert(
                                slot.to_string(),
                                [
                                    pos.x as f64 / scale,
                                    pos.y as f64 / scale,
                                    size.width as f64 / scale,
                                    size.height as f64 / scale,
                                ],
                            );
                            if data.last_settings_save.elapsed() > Duration::from_millis(500) {
                                data.last_settings_save = Instant::now();
                                save_settings(&data);
                            }
                        }
                    }
                }
                _ => {}
            }
        })
        .setup(move |app| {
            let _ = fs::create_dir_all(notes_dir());
            let _ = fs::create_dir_all(commands_dir());
            startup_cleanup();

            {
                let st: tauri::State<State> = app.state();
                let mut data = lockd(&st);
                data.settings = load_settings();
            }

            // watcher thread: only reads state briefly and schedules main-thread
            // work — it never creates windows or touches notify itself
            let handle = app.handle().clone();
            let watcher_state: State = app.state::<State>().inner().clone();
            let generations: Arc<[AtomicU64; 2]> =
                Arc::new([AtomicU64::new(0), AtomicU64::new(0)]);
            let watcher = RecommendedWatcher::new(
                move |res: Result<notify::Event, notify::Error>| {
                    let event = match res {
                        Ok(e) => e,
                        Err(_) => return,
                    };
                    let cmds = canon(&commands_dir());
                    let mut touched_cmds = false;
                    let mut touched_slots: Vec<u8> = Vec::new();
                    {
                        let data = lockd(&watcher_state);
                        for p in &event.paths {
                            if p.parent().map(|d| canon(d) == cmds).unwrap_or(false) {
                                touched_cmds = true;
                            }
                            let cp = canon(p);
                            for slot in 1u8..=2 {
                                let info = &data.slots[(slot - 1) as usize];
                                if !info.path.is_empty()
                                    && canon(Path::new(&info.path)) == cp
                                    && !touched_slots.contains(&slot)
                                {
                                    touched_slots.push(slot);
                                }
                            }
                        }
                    }
                    if touched_cmds {
                        schedule_inbox(&handle);
                    }
                    for slot in touched_slots {
                        // per-slot debounce: coalesce a burst of write events into
                        // one refresh; only the newest pending thread emits
                        let g_arr = generations.clone();
                        let g = g_arr[(slot - 1) as usize].fetch_add(1, Ordering::SeqCst) + 1;
                        let h2 = handle.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(Duration::from_millis(120));
                            if g_arr[(slot - 1) as usize].load(Ordering::SeqCst) == g {
                                emit_note(&h2, slot);
                            }
                        });
                    }
                },
                notify::Config::default(),
            )?;
            {
                let wt: tauri::State<Watch> = app.state();
                lockd(&wt).watcher = Some(watcher);
            }
            sync_watches(app.handle());

            process_inbox(app.handle());

            // manual launch with nothing to show → welcome window on slot 1
            let has_open = {
                let st: tauri::State<State> = app.state();
                let data = lockd(&st);
                data.slots.iter().any(|s| s.state == "open" || s.state == "docked")
            };
            if !has_open {
                ensure_window(app.handle(), 1);
            }
            {
                let st: tauri::State<State> = app.state();
                let data = lockd(&st);
                write_state(&data);
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error building lite-notes");

    let exit_state = state;
    app.run(move |_app, event| {
        if let tauri::RunEvent::Exit = event {
            final_cleanup(&exit_state);
        }
    });
}
