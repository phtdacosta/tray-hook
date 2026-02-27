//! tray-hook — rock-solid, cross-platform system tray daemon
//! driven entirely by JSON over stdin/stdout.
//!
//! ── Inbound (stdin → tray-hook) ───────────────────────────────────────────
//!   { "cmd_id":"c1", "action":"add",         "id":"quit", "title":"Quit" }
//!   { "cmd_id":"c2", "action":"add_check",   "id":"dark", "title":"Dark mode", "checked":false }
//!   { "cmd_id":"c3", "action":"add_submenu", "id":"sub",  "title":"More" }
//!   { "cmd_id":"c4", "action":"add",         "id":"s1",   "title":"Sub-item", "parent_id":"sub" }
//!   { "cmd_id":"c5", "action":"add_separator","id":"sep1" }
//!   { "cmd_id":"c6", "action":"rename",      "id":"quit", "title":"Exit" }
//!   { "cmd_id":"c7", "action":"set_enabled",  "id":"quit", "enabled":false }
//!   { "cmd_id":"c8", "action":"toggle",       "id":"dark" }
//!   { "cmd_id":"c9", "action":"set_checked",  "id":"dark", "checked":true }
//!   { "cmd_id":"c0", "action":"remove",       "id":"sep1" }
//!   {               "action":"clear" }
//!   {               "action":"set_icon",      "path":"/abs/path/icon.png" }
//!   {               "action":"set_tooltip",   "title":"My App" }
//!   {               "action":"set_tray_title","title":"●" }   ← macOS only
//!   {               "action":"quit" }
//!
//! ── Outbound (tray-hook → stdout) ─────────────────────────────────────────
//!   { "event":"ready" }
//!   { "event":"click", "id":"quit" }
//!   { "event":"check", "id":"dark", "checked":true }
//!   { "event":"ack",   "cmd_id":"c1" }
//!   { "event":"error", "cmd_id":"c2", "message":"id 'dark' already exists" }

use muda::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::thread;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{Icon, TrayIconBuilder};

// ─── Constants ────────────────────────────────────────────────────────────────

const MAX_ID_LEN: usize = 128;
const MAX_TITLE_LEN: usize = 256;
const MAX_DEPTH: usize = 5;

// ─── Protocol ─────────────────────────────────────────────────────────────────

/// Every message arriving from the host process.
#[derive(Deserialize, Debug)]
struct BunCommand {
    /// Caller-supplied correlation token; echoed verbatim in ack/error.
    cmd_id: Option<String>,
    action: String,
    id: Option<String>,
    title: Option<String>,
    path: Option<String>,
    enabled: Option<bool>,
    checked: Option<bool>,
    parent_id: Option<String>,
}

/// Every message we push back to the host process.
#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum TrayEvent {
    /// A plain menu item was activated.
    Click { id: String },
    /// A check-item was activated; carries the *new* state after the click.
    Check { id: String, checked: bool },
    /// Command succeeded.
    Ack {
        #[serde(skip_serializing_if = "Option::is_none")]
        cmd_id: Option<String>,
    },
    /// Command failed; `message` is human-readable.
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        cmd_id: Option<String>,
        message: String,
    },
    /// Emitted once the event loop is running and ready to accept commands.
    Ready,
}

/// Write a JSON event to stdout. Never panics; a serialisation failure is silently
/// dropped because there is nowhere meaningful to report it.
fn emit(ev: &TrayEvent) {
    if let Ok(json) = serde_json::to_string(ev) {
        println!("{}", json);
        let _ = io::stdout().flush();
    }
}

// ─── Validation ───────────────────────────────────────────────────────────────

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return Err(format!("id must be 1–{MAX_ID_LEN} characters"));
    }
    if id.chars().any(|c| c.is_control()) {
        return Err("id must not contain control characters".into());
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<(), String> {
    if title.is_empty() || title.len() > MAX_TITLE_LEN {
        return Err(format!("title must be 1–{MAX_TITLE_LEN} characters"));
    }
    Ok(())
}

// ─── Icon Loading ─────────────────────────────────────────────────────────────

fn load_icon(path: &Path) -> Result<Icon, String> {
    if !path.exists() {
        return Err(format!("icon path does not exist: '{}'", path.display()));
    }
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_lowercase())
        .as_deref()
    {
        Some("png" | "ico" | "jpg" | "jpeg") => {}
        other => return Err(format!("unsupported icon format: {:?}", other)),
    }
    let img = image::open(path)
        .map_err(|e| format!("could not open '{}': {}", path.display(), e))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).map_err(|e| format!("invalid icon data: {}", e))
}

// ─── Menu Registry ────────────────────────────────────────────────────────────

enum MenuItemKind {
    Regular(MenuItem),
    Check(CheckMenuItem),
    Submenu(Submenu),
    Separator(PredefinedMenuItem),
}

impl MenuItemKind {
    fn as_item(&self) -> &dyn IsMenuItem {
        match self {
            Self::Regular(i) => i,
            Self::Check(i) => i,
            Self::Submenu(i) => i,
            Self::Separator(i) => i,
        }
    }
}

struct MenuEntry {
    kind: MenuItemKind,
    parent_id: Option<String>,
    depth: usize,
}

/// Single source of truth for all runtime menu state.
///
/// ## Borrow-safety design note
///
/// `submenus` mirrors every `MenuItemKind::Submenu` that is also stored in
/// `entries`.  Keeping them in a *separate field* means we can simultaneously
/// hold an immutable borrow of `entries[id].kind` (to get the `&dyn
/// IsMenuItem` needed for muda's remove call) and an immutable borrow of
/// `submenus[parent_id]` (to call `Submenu::remove`), which would otherwise
/// be a double-borrow on the same `HashMap`.
struct MenuRegistry {
    entries: HashMap<String, MenuEntry>,
    submenus: HashMap<String, Submenu>,
    root: Menu,
}

impl MenuRegistry {
    fn new(root: Menu) -> Self {
        Self {
            entries: HashMap::new(),
            submenus: HashMap::new(),
            root,
        }
    }

    // ── guard helpers ─────────────────────────────────────────────────────

    fn guard_dup(&self, id: &str) -> Result<(), String> {
        if self.entries.contains_key(id) {
            Err(format!("id '{}' already exists", id))
        } else {
            Ok(())
        }
    }

    fn guard_depth(&self, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH {
            Err(format!("max nesting depth of {MAX_DEPTH} exceeded"))
        } else {
            Ok(())
        }
    }

    fn resolve_depth(&self, parent_id: &Option<String>) -> usize {
        parent_id
            .as_ref()
            .and_then(|pid| self.entries.get(pid))
            .map(|e| e.depth + 1)
            .unwrap_or(0)
    }

    // ── internal muda plumbing ────────────────────────────────────────────

    fn append_to(&self, item: &dyn IsMenuItem, parent_id: &Option<String>) -> Result<(), String> {
        match parent_id {
            Some(pid) => self
                .submenus
                .get(pid)
                .ok_or_else(|| format!("'{}' is not a submenu or does not exist", pid))?
                .append(item)
                .map_err(|e| e.to_string()),
            None => self.root.append(item).map_err(|e| e.to_string()),
        }
    }

    fn detach_from(&self, item: &dyn IsMenuItem, parent_id: &Option<String>) -> Result<(), String> {
        match parent_id {
            Some(pid) => self
                .submenus
                .get(pid)
                .ok_or_else(|| format!("parent submenu '{}' not found", pid))?
                .remove(item)
                .map_err(|e| e.to_string()),
            None => self.root.remove(item).map_err(|e| e.to_string()),
        }
    }

    // ── add operations ────────────────────────────────────────────────────

    fn add_regular(
        &mut self,
        id: String,
        title: String,
        enabled: bool,
        parent_id: Option<String>,
    ) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        self.guard_depth(depth)?;
        let item = MenuItem::with_id(id.clone(), &title, enabled, None);
        self.append_to(&item, &parent_id)?;
        self.entries
            .insert(id, MenuEntry { kind: MenuItemKind::Regular(item), parent_id, depth });
        Ok(())
    }

    fn add_check(
        &mut self,
        id: String,
        title: String,
        enabled: bool,
        checked: bool,
        parent_id: Option<String>,
    ) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        self.guard_depth(depth)?;
        let item = CheckMenuItem::with_id(id.clone(), &title, enabled, checked, None);
        self.append_to(&item, &parent_id)?;
        self.entries
            .insert(id, MenuEntry { kind: MenuItemKind::Check(item), parent_id, depth });
        Ok(())
    }

    fn add_submenu(
        &mut self,
        id: String,
        title: String,
        enabled: bool,
        parent_id: Option<String>,
    ) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        self.guard_depth(depth)?;
        let sub = Submenu::with_id(id.clone(), &title, enabled);
        self.append_to(&sub, &parent_id)?;
        // Arc-backed clone — both copies share the same underlying object.
        self.submenus.insert(id.clone(), sub.clone());
        self.entries
            .insert(id, MenuEntry { kind: MenuItemKind::Submenu(sub), parent_id, depth });
        Ok(())
    }

    fn add_separator(&mut self, id: String, parent_id: Option<String>) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        let sep = PredefinedMenuItem::separator();
        self.append_to(&sep, &parent_id)?;
        self.entries
            .insert(id, MenuEntry { kind: MenuItemKind::Separator(sep), parent_id, depth });
        Ok(())
    }

    // ── mutation operations ───────────────────────────────────────────────

    fn rename(&self, id: &str, title: String) -> Result<(), String> {
        let entry = self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?;
        match &entry.kind {
            MenuItemKind::Regular(i) => i.set_text(&title),
            MenuItemKind::Check(i) => i.set_text(&title),
            MenuItemKind::Submenu(i) => i.set_text(&title),
            MenuItemKind::Separator(_) => return Err("separators have no title".into()),
        }
        Ok(())
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let entry = self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?;
        match &entry.kind {
            MenuItemKind::Regular(i) => i.set_enabled(enabled),
            MenuItemKind::Check(i) => i.set_enabled(enabled),
            MenuItemKind::Submenu(i) => i.set_enabled(enabled),
            MenuItemKind::Separator(_) => return Err("separators have no enabled state".into()),
        }
        Ok(())
    }

    fn set_checked(&self, id: &str, checked: bool) -> Result<(), String> {
        let entry = self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?;
        match &entry.kind {
            MenuItemKind::Check(i) => { i.set_checked(checked); Ok(()) }
            _ => Err(format!("'{}' is not a check item", id)),
        }
    }

    fn toggle(&self, id: &str) -> Result<bool, String> {
        let entry = self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?;
        match &entry.kind {
            MenuItemKind::Check(i) => {
                let next = !i.is_checked();
                i.set_checked(next);
                Ok(next)
            }
            _ => Err(format!("'{}' is not a check item", id)),
        }
    }

    /// Returns the current checked state if `id` is a check item, else None.
    fn check_state(&self, id: &str) -> Option<bool> {
        match &self.entries.get(id)?.kind {
            MenuItemKind::Check(i) => Some(i.is_checked()),
            _ => None,
        }
    }

    /// Remove a single item.  Submenus must be emptied first.
    fn remove(&mut self, id: &str) -> Result<(), String> {
        if !self.entries.contains_key(id) {
            return Err(format!("'{}' not found", id));
        }
        // Refuse to leave children orphaned in muda's internal tree.
        if self.entries.values().any(|e| e.parent_id.as_deref() == Some(id)) {
            return Err(format!(
                "'{}' still has children — remove them first",
                id
            ));
        }
        let parent_id = self.entries[id].parent_id.clone();
        {
            // entries[id] → immutable borrow; submenus[parent] is a *different field* → safe.
            let item_ref = self.entries[id].kind.as_item();
            self.detach_from(item_ref, &parent_id)?;
        }
        self.submenus.remove(id);
        self.entries.remove(id);
        Ok(())
    }

    /// Wipe every item from the menu and the registry atomically.
    fn clear(&mut self) -> Result<(), String> {
        // Collect root-level IDs up-front to avoid holding a borrow during removal.
        let root_ids: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, e)| e.parent_id.is_none())
            .map(|(id, _)| id.clone())
            .collect();

        // Removing a root-level submenu also removes all of its descendants
        // from the OS-side menu, so we only need to touch root items here.
        // Best-effort — if muda returns an error the registry is still cleared.
        for id in &root_ids {
            let item_ref = self.entries[id].kind.as_item();
            let _ = self.root.remove(item_ref);
        }

        self.entries.clear();
        self.submenus.clear();
        Ok(())
    }
}

// ─── Command Dispatch ─────────────────────────────────────────────────────────

enum Outcome {
    Ok,
    Quit,
}

fn process(
    cmd: BunCommand,
    reg: &mut MenuRegistry,
    tray: &mut tray_icon::TrayIcon,
) -> Result<Outcome, String> {
    match cmd.action.as_str() {
        // ── tray-level controls ───────────────────────────────────────────
        "set_icon" => {
            let p = cmd.path.ok_or("missing 'path'")?;
            let icon = load_icon(Path::new(&p))?;
            tray.set_icon(Some(icon)).map_err(|e| e.to_string())?;
        }
        "set_tooltip" => {
            let t = cmd.title.ok_or("missing 'title'")?;
            validate_title(&t)?;
            tray.set_tooltip(Some(t)).map_err(|e| e.to_string())?;
        }
        // macOS-only: text shown beside the tray icon in the menu bar.
        "set_tray_title" => {
            #[cfg(target_os = "macos")]
            {
                let t = cmd.title.ok_or("missing 'title'")?;
                validate_title(&t)?;
                tray.set_title(Some(t));
            }
            #[cfg(not(target_os = "macos"))]
            {
                return Err("set_tray_title is only supported on macOS".into());
            }
        }

        // ── item creation ─────────────────────────────────────────────────
        "add" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_id(&id)?;
            validate_title(&title)?;
            reg.add_regular(id, title, cmd.enabled.unwrap_or(true), cmd.parent_id)?;
        }
        "add_check" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_id(&id)?;
            validate_title(&title)?;
            reg.add_check(
                id,
                title,
                cmd.enabled.unwrap_or(true),
                cmd.checked.unwrap_or(false),
                cmd.parent_id,
            )?;
        }
        "add_submenu" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_id(&id)?;
            validate_title(&title)?;
            reg.add_submenu(id, title, cmd.enabled.unwrap_or(true), cmd.parent_id)?;
        }
        "add_separator" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            validate_id(&id)?;
            reg.add_separator(id, cmd.parent_id)?;
        }

        // ── item mutation ─────────────────────────────────────────────────
        "rename" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_title(&title)?;
            reg.rename(&id, title)?;
        }
        "set_enabled" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            let en = cmd.enabled.ok_or("missing 'enabled'")?;
            reg.set_enabled(&id, en)?;
        }
        "set_checked" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            let ch = cmd.checked.ok_or("missing 'checked'")?;
            reg.set_checked(&id, ch)?;
        }
        "toggle" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            reg.toggle(&id)?;
        }
        "remove" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            reg.remove(&id)?;
        }
        "clear" => reg.clear()?,

        // ── lifecycle ─────────────────────────────────────────────────────
        "quit" => return Ok(Outcome::Quit),

        other => return Err(format!("unknown action '{}'", other)),
    }

    Ok(Outcome::Ok)
}

// ─── Entry Point ──────────────────────────────────────────────────────────────

fn main() {
    let event_loop = EventLoopBuilder::<BunCommand>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Stdin reader — runs on its own thread, completely decoupled from the GUI.
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(raw) if !raw.trim().is_empty() => {
                    match serde_json::from_str::<BunCommand>(&raw) {
                        Ok(cmd) => {
                            if proxy.send_event(cmd).is_err() {
                                break; // event loop already exited
                            }
                        }
                        Err(e) => emit(&TrayEvent::Error {
                            cmd_id: None,
                            message: format!("JSON parse error: {}", e),
                        }),
                    }
                }
                Ok(_) => {}       // blank line — ignore
                Err(_) => break,  // stdin closed or broken pipe
            }
        }
        // stdin EOF → host process is gone, exit cleanly.
        // The event loop may or may not still be running; calling exit()
        // here is the safest way to guarantee termination without racing.
        std::process::exit(0);
    });

    let root_menu = Menu::new();
    let mut registry = MenuRegistry::new(root_menu.clone());

    let mut tray = match TrayIconBuilder::new()
        .with_menu(Box::new(root_menu.clone()))
        .with_tooltip("tray-hook")
        .build()
    {
        Ok(t) => t,
        Err(e) => {
            emit(&TrayEvent::Error {
                cmd_id: None,
                message: format!("failed to initialise tray icon: {}", e),
            });
            std::process::exit(1);
        }
    };

    let menu_rx = MenuEvent::receiver();

    // Tell the host we're live and ready.
    emit(&TrayEvent::Ready);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // ── 1. Forward click/check events to the host ──────────────────────
        // Drain the entire channel each frame so no events are dropped under
        // rapid successive clicks.
        while let Ok(ev) = menu_rx.try_recv() {
            let id = ev.id.0.clone();
            // Distinguish check items so the host receives rich state data.
            if let Some(checked) = registry.check_state(&id) {
                emit(&TrayEvent::Check { id, checked });
            } else {
                emit(&TrayEvent::Click { id });
            }
        }

        // ── 2. Execute inbound commands ────────────────────────────────────
        if let Event::UserEvent(cmd) = event {
            let cmd_id = cmd.cmd_id.clone();
            match process(cmd, &mut registry, &mut tray) {
                Ok(Outcome::Quit) => *control_flow = ControlFlow::Exit,
                Ok(Outcome::Ok) => emit(&TrayEvent::Ack { cmd_id }),
                Err(msg) => emit(&TrayEvent::Error { cmd_id, message: msg }),
            }
        }
    });
}