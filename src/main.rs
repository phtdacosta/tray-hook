//! tray-hook — cross-platform system tray daemon driven by JSON over stdin/stdout.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use muda::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::thread;
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

// ─── Constants ────────────────────────────────────────────────────────────────

const MAX_ID_LEN: usize = 128;
const MAX_TITLE_LEN: usize = 256;
const MAX_DEPTH: usize = 5;

// ─── Wire Types (stdin) ───────────────────────────────────────────────────────

/// Raw inbound command before any preprocessing.
#[derive(Deserialize, Debug)]
struct BunCommand {
    cmd_id:     Option<String>,
    action:     String,
    id:         Option<String>,
    title:      Option<String>,
    path:       Option<String>,                   // set_icon: file path
    icon:       Option<String>,                   // add / add_check: item icon path
    data:       Option<String>,                   // set_icon_data: base64 PNG
    enabled:    Option<bool>,
    checked:    Option<bool>,
    parent_id:  Option<String>,
    items:      Option<Vec<RawMenuItem>>,         // set_menu template
    states:     Option<HashMap<String, String>>,  // define_states: name → path
    state_name: Option<String>,                   // set_state
    app_id:     Option<String>,                   // autostart
    exec_path:  Option<String>,                   // autostart
}

/// Raw menu item node from the set_menu template (before icon decoding).
#[derive(Deserialize, Debug)]
struct RawMenuItem {
    #[serde(rename = "type")]
    item_type: String,
    id:        String,
    title:     Option<String>,
    enabled:   Option<bool>,
    checked:   Option<bool>,
    icon:      Option<String>,
    items:     Option<Vec<RawMenuItem>>,
}

// ─── Preprocessed Types (cross-thread) ───────────────────────────────────────

/// Send-safe icon data. tray_icon::Icon holds a platform handle (HICON on
/// Windows) and is therefore !Send. We pass raw RGBA bytes through the
/// EventLoopProxy and reconstruct Icon on the GUI thread just before use.
type RgbaBytes = (Vec<u8>, u32, u32);

/// Menu item node with all icons already decoded to RgbaBytes.
#[derive(Debug)]
struct ProcessedMenuItem {
    item_type: String,
    id:        String,
    title:     Option<String>,
    enabled:   bool,
    checked:   bool,
    icon:      Option<RgbaBytes>,
    items:     Vec<ProcessedMenuItem>,
}

/// Fully preprocessed command — all I/O and decoding done on the stdin thread.
/// Every variant is Send because it contains only Vec<u8>, String, bool, etc.
#[derive(Debug)]
enum ProcessedCommand {
    // Tray level
    SetIcon(RgbaBytes),
    SetIconData(RgbaBytes),
    SetTooltip(String),
    SetTrayTitle(String),
    // Menu creation
    Add { id: String, title: String, enabled: bool, parent_id: Option<String>, icon: Option<RgbaBytes> },
    AddCheck { id: String, title: String, enabled: bool, checked: bool, parent_id: Option<String>, icon: Option<RgbaBytes> },
    AddSubmenu { id: String, title: String, enabled: bool, parent_id: Option<String> },
    AddSeparator { id: String, parent_id: Option<String> },
    SetMenu(Vec<ProcessedMenuItem>),
    // Mutation
    Rename { id: String, title: String },
    SetEnabled { id: String, enabled: bool },
    SetChecked { id: String, checked: bool },
    Toggle(String),
    Remove(String),
    Clear,
    // Icon states
    DefineStates(HashMap<String, RgbaBytes>),
    SetState(String),
    // Autostart (Windows → daemon; macOS/Linux → pure JS)
    SetAutostart { app_id: String, exec_path: String, enabled: bool },
    GetAutostart(String),
    // Lifecycle
    Quit,
}

/// The EventLoop user-event type.
///
/// Using a proper enum instead of a tuple lets us forward `muda::MenuEvent`
/// and `tray_icon::TrayIconEvent` through the same proxy so the event loop
/// wakes up immediately on Windows (which blocks in `WaitMessage` under
/// `ControlFlow::Wait` — crossbeam channel writes alone do not post a Win32
/// message and therefore cannot wake the loop).
enum AppEvent {
    Command(Option<String>, ProcessedCommand),
    Menu(muda::MenuEvent),
    Tray(tray_icon::TrayIconEvent),
}

// ─── Wire Types (stdout) ─────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum TrayEvent {
    Ready,
    Click    { id: String },
    Check    { id: String, checked: bool },
    TrayClick { button: String },
    Ack {
        #[serde(skip_serializing_if = "Option::is_none")]
        cmd_id: Option<String>,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        cmd_id: Option<String>,
        message: String,
    },
    Autostart {
        #[serde(skip_serializing_if = "Option::is_none")]
        cmd_id: Option<String>,
        enabled: bool,
    },
}

enum Outcome {
    Ok,
    Responded, // command emitted its own custom response; suppress Ack
    Quit,
}

// ─── Emit ─────────────────────────────────────────────────────────────────────

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

// ─── Icon helpers ─────────────────────────────────────────────────────────────

fn load_icon_bytes(path: &Path) -> Result<RgbaBytes, String> {
    if !path.exists() {
        return Err(format!("icon path does not exist: '{}'", path.display()));
    }
    match path.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase()).as_deref() {
        Some("png" | "ico" | "jpg" | "jpeg") => {}
        other => return Err(format!("unsupported icon format: {:?}", other)),
    }
    let img = image::open(path)
        .map_err(|e| format!("could not open '{}': {}", path.display(), e))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w, h))
}

fn decode_base64_bytes(data: &str) -> Result<RgbaBytes, String> {
    let bytes = B64.decode(data)
        .map_err(|e| format!("base64 decode error: {}", e))?;
    let img = image::load_from_memory(&bytes)
        .map_err(|e| format!("image decode error: {}", e))?
        .into_rgba8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w, h))
}

/// Reconstruct an Icon from RgbaBytes on the GUI thread.
fn bytes_to_icon((rgba, w, h): RgbaBytes) -> Result<Icon, String> {
    Icon::from_rgba(rgba, w, h).map_err(|e| e.to_string())
}

// ─── Preprocessing (stdin thread) ────────────────────────────────────────────

fn process_raw_menu_item(raw: RawMenuItem) -> Result<ProcessedMenuItem, String> {
    let icon = raw.icon.as_deref()
        .map(|p| load_icon_bytes(Path::new(p)))
        .transpose()?;
    let mut children = Vec::new();
    for child in raw.items.unwrap_or_default() {
        children.push(process_raw_menu_item(child)?);
    }
    Ok(ProcessedMenuItem {
        item_type: raw.item_type,
        id:        raw.id,
        title:     raw.title,
        enabled:   raw.enabled.unwrap_or(true),
        checked:   raw.checked.unwrap_or(false),
        icon,
        items: children,
    })
}

/// Convert a raw BunCommand into a ProcessedCommand, performing all I/O and
/// decoding here on the stdin thread so the GUI thread stays free of blocking.
fn preprocess(cmd: BunCommand) -> Result<ProcessedCommand, String> {
    match cmd.action.as_str() {
        "set_icon" => {
            let p = cmd.path.ok_or("missing 'path'")?;
            Ok(ProcessedCommand::SetIcon(load_icon_bytes(Path::new(&p))?))
        }
        "set_icon_data" => {
            let data = cmd.data.ok_or("missing 'data'")?;
            Ok(ProcessedCommand::SetIconData(decode_base64_bytes(&data)?))
        }
        "set_tooltip" => {
            let t = cmd.title.ok_or("missing 'title'")?;
            validate_title(&t)?;
            Ok(ProcessedCommand::SetTooltip(t))
        }
        "set_tray_title" => {
            let t = cmd.title.ok_or("missing 'title'")?;
            validate_title(&t)?;
            Ok(ProcessedCommand::SetTrayTitle(t))
        }
        "add" => {
            let id    = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_id(&id)?;
            validate_title(&title)?;
            let icon = cmd.icon.as_deref().map(|p| load_icon_bytes(Path::new(p))).transpose()?;
            Ok(ProcessedCommand::Add {
                id, title,
                enabled:   cmd.enabled.unwrap_or(true),
                parent_id: cmd.parent_id,
                icon,
            })
        }
        "add_check" => {
            let id    = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_id(&id)?;
            validate_title(&title)?;
            let icon = cmd.icon.as_deref().map(|p| load_icon_bytes(Path::new(p))).transpose()?;
            Ok(ProcessedCommand::AddCheck {
                id, title,
                enabled:   cmd.enabled.unwrap_or(true),
                checked:   cmd.checked.unwrap_or(false),
                parent_id: cmd.parent_id,
                icon,
            })
        }
        "add_submenu" => {
            let id    = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_id(&id)?;
            validate_title(&title)?;
            Ok(ProcessedCommand::AddSubmenu {
                id, title,
                enabled:   cmd.enabled.unwrap_or(true),
                parent_id: cmd.parent_id,
            })
        }
        "add_separator" => {
            let id = cmd.id.ok_or("missing 'id'")?;
            validate_id(&id)?;
            Ok(ProcessedCommand::AddSeparator { id, parent_id: cmd.parent_id })
        }
        "set_menu" => {
            let raw_items = cmd.items.ok_or("missing 'items'")?;
            let mut processed = Vec::new();
            for item in raw_items {
                processed.push(process_raw_menu_item(item)?);
            }
            Ok(ProcessedCommand::SetMenu(processed))
        }
        "rename" => {
            let id    = cmd.id.ok_or("missing 'id'")?;
            let title = cmd.title.ok_or("missing 'title'")?;
            validate_title(&title)?;
            Ok(ProcessedCommand::Rename { id, title })
        }
        "set_enabled" => {
            let id      = cmd.id.ok_or("missing 'id'")?;
            let enabled = cmd.enabled.ok_or("missing 'enabled'")?;
            Ok(ProcessedCommand::SetEnabled { id, enabled })
        }
        "set_checked" => {
            let id      = cmd.id.ok_or("missing 'id'")?;
            let checked = cmd.checked.ok_or("missing 'checked'")?;
            Ok(ProcessedCommand::SetChecked { id, checked })
        }
        "toggle"  => Ok(ProcessedCommand::Toggle(cmd.id.ok_or("missing 'id'")?)),
        "remove"  => Ok(ProcessedCommand::Remove(cmd.id.ok_or("missing 'id'")?)),
        "clear"   => Ok(ProcessedCommand::Clear),
        "define_states" => {
            let map = cmd.states.ok_or("missing 'states'")?;
            let mut decoded = HashMap::new();
            for (name, path_str) in map {
                decoded.insert(name, load_icon_bytes(Path::new(&path_str))?);
            }
            Ok(ProcessedCommand::DefineStates(decoded))
        }
        "set_state"    => Ok(ProcessedCommand::SetState(cmd.state_name.ok_or("missing 'state_name'")?)),
        "set_autostart" => {
            let app_id    = cmd.app_id.ok_or("missing 'app_id'")?;
            let exec_path = cmd.exec_path.ok_or("missing 'exec_path'")?;
            let enabled   = cmd.enabled.ok_or("missing 'enabled'")?;
            Ok(ProcessedCommand::SetAutostart { app_id, exec_path, enabled })
        }
        "get_autostart" => Ok(ProcessedCommand::GetAutostart(cmd.app_id.ok_or("missing 'app_id'")?)),
        "quit"          => Ok(ProcessedCommand::Quit),
        other           => Err(format!("unknown action '{}'", other)),
    }
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
            Self::Regular(i)   => i,
            Self::Check(i)     => i,
            Self::Submenu(i)   => i,
            Self::Separator(i) => i,
        }
    }
}

struct MenuEntry {
    kind:      MenuItemKind,
    parent_id: Option<String>,
    depth:     usize,
}

struct MenuRegistry {
    entries:  HashMap<String, MenuEntry>,
    submenus: HashMap<String, Submenu>,
    root:     Menu,
}

impl MenuRegistry {
    fn new(root: Menu) -> Self {
        Self { entries: HashMap::new(), submenus: HashMap::new(), root }
    }

    fn guard_dup(&self, id: &str) -> Result<(), String> {
        if self.entries.contains_key(id) { Err(format!("id '{}' already exists", id)) }
        else { Ok(()) }
    }

    fn guard_depth(&self, depth: usize) -> Result<(), String> {
        if depth > MAX_DEPTH { Err(format!("max nesting depth of {MAX_DEPTH} exceeded")) }
        else { Ok(()) }
    }

    fn resolve_depth(&self, parent_id: &Option<String>) -> usize {
        parent_id.as_ref()
            .and_then(|pid| self.entries.get(pid))
            .map(|e| e.depth + 1)
            .unwrap_or(0)
    }

    fn append_to(&self, item: &dyn IsMenuItem, parent_id: &Option<String>) -> Result<(), String> {
        match parent_id {
            Some(pid) => self.submenus.get(pid)
                .ok_or_else(|| format!("'{}' is not a submenu or does not exist", pid))?
                .append(item).map_err(|e| e.to_string()),
            None => self.root.append(item).map_err(|e| e.to_string()),
        }
    }

    fn detach_from(&self, item: &dyn IsMenuItem, parent_id: &Option<String>) -> Result<(), String> {
        match parent_id {
            Some(pid) => self.submenus.get(pid)
                .ok_or_else(|| format!("parent submenu '{}' not found", pid))?
                .remove(item).map_err(|e| e.to_string()),
            None => self.root.remove(item).map_err(|e| e.to_string()),
        }
    }

    fn add_regular(&mut self, id: String, title: String, enabled: bool, parent_id: Option<String>, icon: Option<RgbaBytes>) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        self.guard_depth(depth)?;
        let _ = icon; // muda 0.17 does not expose set_icon on MenuItem
        let item = MenuItem::with_id(id.clone(), &title, enabled, None);
        self.append_to(&item, &parent_id)?;
        self.entries.insert(id, MenuEntry { kind: MenuItemKind::Regular(item), parent_id, depth });
        Ok(())
    }

    fn add_check(&mut self, id: String, title: String, enabled: bool, checked: bool, parent_id: Option<String>, icon: Option<RgbaBytes>) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        self.guard_depth(depth)?;
        let item = CheckMenuItem::with_id(id.clone(), &title, enabled, checked, None);
        // muda 0.17 CheckMenuItem does not expose set_icon; icon field is accepted
        // in the protocol for forward-compatibility but silently ignored here.
        let _ = icon;
        self.append_to(&item, &parent_id)?;
        self.entries.insert(id, MenuEntry { kind: MenuItemKind::Check(item), parent_id, depth });
        Ok(())
    }

    fn add_submenu(&mut self, id: String, title: String, enabled: bool, parent_id: Option<String>) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        self.guard_depth(depth)?;
        let sub = Submenu::with_id(id.clone(), &title, enabled);
        self.append_to(&sub, &parent_id)?;
        self.submenus.insert(id.clone(), sub.clone());
        self.entries.insert(id, MenuEntry { kind: MenuItemKind::Submenu(sub), parent_id, depth });
        Ok(())
    }

    fn add_separator(&mut self, id: String, parent_id: Option<String>) -> Result<(), String> {
        self.guard_dup(&id)?;
        let depth = self.resolve_depth(&parent_id);
        let sep = PredefinedMenuItem::separator();
        self.append_to(&sep, &parent_id)?;
        self.entries.insert(id, MenuEntry { kind: MenuItemKind::Separator(sep), parent_id, depth });
        Ok(())
    }

    fn rename(&self, id: &str, title: String) -> Result<(), String> {
        let entry = self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?;
        match &entry.kind {
            MenuItemKind::Regular(i)   => i.set_text(&title),
            MenuItemKind::Check(i)     => i.set_text(&title),
            MenuItemKind::Submenu(i)   => i.set_text(&title),
            MenuItemKind::Separator(_) => return Err("separators have no title".into()),
        }
        Ok(())
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let entry = self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?;
        match &entry.kind {
            MenuItemKind::Regular(i)   => i.set_enabled(enabled),
            MenuItemKind::Check(i)     => i.set_enabled(enabled),
            MenuItemKind::Submenu(i)   => i.set_enabled(enabled),
            MenuItemKind::Separator(_) => return Err("separators have no enabled state".into()),
        }
        Ok(())
    }

    fn set_checked(&self, id: &str, checked: bool) -> Result<(), String> {
        match &self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?.kind {
            MenuItemKind::Check(i) => { i.set_checked(checked); Ok(()) }
            _ => Err(format!("'{}' is not a check item", id)),
        }
    }

    fn check_state(&self, id: &str) -> Option<bool> {
        match &self.entries.get(id)?.kind {
            MenuItemKind::Check(i) => Some(i.is_checked()),
            _ => None,
        }
    }

    fn toggle(&self, id: &str) -> Result<bool, String> {
        match &self.entries.get(id).ok_or_else(|| format!("'{}' not found", id))?.kind {
            MenuItemKind::Check(i) => { let next = !i.is_checked(); i.set_checked(next); Ok(next) }
            _ => Err(format!("'{}' is not a check item", id)),
        }
    }

    fn remove(&mut self, id: &str) -> Result<(), String> {
        if !self.entries.contains_key(id) {
            return Err(format!("'{}' not found", id));
        }
        if self.entries.values().any(|e| e.parent_id.as_deref() == Some(id)) {
            return Err(format!("'{}' still has children — remove them first", id));
        }
        let parent_id = self.entries[id].parent_id.clone();
        {
            let item_ref = self.entries[id].kind.as_item();
            self.detach_from(item_ref, &parent_id)?;
        }
        self.submenus.remove(id);
        self.entries.remove(id);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), String> {
        let root_ids: Vec<String> = self.entries.iter()
            .filter(|(_, e)| e.parent_id.is_none())
            .map(|(id, _)| id.clone())
            .collect();
        for id in &root_ids {
            let item_ref = self.entries[id].kind.as_item();
            let _ = self.root.remove(item_ref);
        }
        self.entries.clear();
        self.submenus.clear();
        Ok(())
    }
}

// ─── set_menu recursive builder ───────────────────────────────────────────────

fn build_menu_items(
    items:            Vec<ProcessedMenuItem>,
    root:             &Menu,
    parent_sub:       Option<(&str, &Submenu)>,
    entries:          &mut HashMap<String, MenuEntry>,
    submenus:         &mut HashMap<String, Submenu>,
    depth:            usize,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("max nesting depth of {MAX_DEPTH} exceeded"));
    }
    let parent_id = parent_sub.map(|(id, _)| id.to_string());

    for item in items {
        // Append to parent submenu or root
        // Explicit type annotation required — Rust cannot infer the closure
        // parameter type when IsMenuItem is a trait object behind a reference.
        let append = |mi: &(dyn IsMenuItem + 'static)| -> Result<(), String> {
            if let Some((_, sub)) = parent_sub {
                sub.append(mi).map_err(|e| e.to_string())
            } else {
                root.append(mi).map_err(|e| e.to_string())
            }
        };

        match item.item_type.as_str() {
            "item" => {
                let title = item.title.unwrap_or_default();
                let _icon = item.icon; // muda 0.17 does not expose set_icon on MenuItem
                let mi = MenuItem::with_id(item.id.clone(), &title, item.enabled, None);
                append(&mi)?;
                entries.insert(item.id, MenuEntry { kind: MenuItemKind::Regular(mi), parent_id: parent_id.clone(), depth });
            }
            "check" => {
                let title = item.title.unwrap_or_default();
                let _icon = item.icon; // muda 0.17 does not expose set_icon on CheckMenuItem
                let ci = CheckMenuItem::with_id(item.id.clone(), &title, item.enabled, item.checked, None);
                append(&ci)?;
                entries.insert(item.id, MenuEntry { kind: MenuItemKind::Check(ci), parent_id: parent_id.clone(), depth });
            }
            "separator" => {
                let sep = PredefinedMenuItem::separator();
                append(&sep)?;
                entries.insert(item.id, MenuEntry { kind: MenuItemKind::Separator(sep), parent_id: parent_id.clone(), depth });
            }
            "submenu" => {
                let id       = item.id;
                let title    = item.title.unwrap_or_default();
                let enabled  = item.enabled;
                let children = item.items;
                let sub = Submenu::with_id(id.clone(), &title, enabled);
                append(&sub)?;
                submenus.insert(id.clone(), sub.clone());
                entries.insert(id.clone(), MenuEntry {
                    kind: MenuItemKind::Submenu(sub.clone()),
                    parent_id: parent_id.clone(),
                    depth,
                });
                build_menu_items(children, root, Some((&id, &sub)), entries, submenus, depth + 1)?;
            }
            other => return Err(format!("unknown menu item type '{}'", other)),
        }
    }
    Ok(())
}

// ─── Windows autostart ────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
fn set_autostart_windows(app_id: &str, exec_path: &str, enabled: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
    use winreg::RegKey;
    let run = RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_SET_VALUE)
        .map_err(|e| format!("registry open error: {}", e))?;
    if enabled {
        run.set_value(app_id, &exec_path.to_string())
            .map_err(|e| format!("registry write error: {}", e))?;
    } else {
        let _ = run.delete_value(app_id); // not-found is not an error
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn get_autostart_windows(app_id: &str) -> Result<bool, String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE};
    use winreg::RegKey;
    match RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey_with_flags(r"Software\Microsoft\Windows\CurrentVersion\Run", KEY_QUERY_VALUE)
    {
        Ok(run) => Ok(run.get_value::<String, _>(app_id).is_ok()),
        Err(_)  => Ok(false),
    }
}

// ─── Command Dispatch ─────────────────────────────────────────────────────────

fn process(
    cmd_id:     &Option<String>,
    cmd:        ProcessedCommand,
    registry:   &mut MenuRegistry,
    tray:       &mut tray_icon::TrayIcon,
    states_map: &mut HashMap<String, RgbaBytes>,
) -> Result<Outcome, String> {
    match cmd {
        ProcessedCommand::SetIcon(b) => {
            tray.set_icon(Some(bytes_to_icon(b)?)).map_err(|e| e.to_string())?;
        }
        ProcessedCommand::SetIconData(b) => {
            tray.set_icon(Some(bytes_to_icon(b)?)).map_err(|e| e.to_string())?;
        }
        ProcessedCommand::SetTooltip(t) => {
            tray.set_tooltip(Some(t)).map_err(|e| e.to_string())?;
        }
        ProcessedCommand::SetTrayTitle(t) => {
            #[cfg(target_os = "macos")]
            tray.set_title(Some(t));
            #[cfg(not(target_os = "macos"))]
            { let _ = t; return Err("set_tray_title is only supported on macOS".into()); }
        }
        ProcessedCommand::Add { id, title, enabled, parent_id, icon } => {
            registry.add_regular(id, title, enabled, parent_id, icon)?;
        }
        ProcessedCommand::AddCheck { id, title, enabled, checked, parent_id, icon } => {
            registry.add_check(id, title, enabled, checked, parent_id, icon)?;
        }
        ProcessedCommand::AddSubmenu { id, title, enabled, parent_id } => {
            registry.add_submenu(id, title, enabled, parent_id)?;
        }
        ProcessedCommand::AddSeparator { id, parent_id } => {
            registry.add_separator(id, parent_id)?;
        }
        ProcessedCommand::SetMenu(items) => {
            let new_root = Menu::new();
            let mut new_entries  = HashMap::new();
            let mut new_submenus = HashMap::new();
            build_menu_items(items, &new_root, None, &mut new_entries, &mut new_submenus, 0)?;
            // Atomically swap the OS-level menu first, then drop old registry data.
                            // tray_icon 0.21 set_menu returns () — no Result to propagate.
                tray.set_menu(Some(Box::new(new_root.clone())));
            registry.entries  = new_entries;
            registry.submenus = new_submenus;
            registry.root     = new_root;
        }
        ProcessedCommand::Rename { id, title }     => { registry.rename(&id, title)?; }
        ProcessedCommand::SetEnabled { id, enabled } => { registry.set_enabled(&id, enabled)?; }
        ProcessedCommand::SetChecked { id, checked } => { registry.set_checked(&id, checked)?; }
        ProcessedCommand::Toggle(id) => { registry.toggle(&id)?; }
        ProcessedCommand::Remove(id) => { registry.remove(&id)?; }
        ProcessedCommand::Clear      => { registry.clear()?; }
        ProcessedCommand::DefineStates(states) => {
            *states_map = states;
        }
        ProcessedCommand::SetState(name) => {
            let bytes = states_map.get(&name).cloned()
                .ok_or_else(|| format!("state '{}' is not defined — call define_states first", name))?;
            tray.set_icon(Some(bytes_to_icon(bytes)?)).map_err(|e| e.to_string())?;
        }
        ProcessedCommand::SetAutostart { app_id, exec_path, enabled } => {
            #[cfg(target_os = "windows")]
            set_autostart_windows(&app_id, &exec_path, enabled)?;
            #[cfg(not(target_os = "windows"))]
            return Err("set_autostart should be handled in JS on non-Windows".into());
        }
        ProcessedCommand::GetAutostart(app_id) => {
            #[cfg(target_os = "windows")] {
                let enabled = get_autostart_windows(&app_id)?;
                emit(&TrayEvent::Autostart { cmd_id: cmd_id.clone(), enabled });
                return Ok(Outcome::Responded);
            }
            #[cfg(not(target_os = "windows"))]
            return Err("get_autostart should be handled in JS on non-Windows".into());
        }
        ProcessedCommand::Quit => return Ok(Outcome::Quit),
    }
    Ok(Outcome::Ok)
}

// ─── Entry Point ──────────────────────────────────────────────────────────────

fn main() {
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Stdin reader — runs on its own OS thread.
    // All I/O (icon file reads, base64 decoding) happens here.
    thread::spawn(move || {
        let stdin = io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(raw) if !raw.trim().is_empty() => {
                    match serde_json::from_str::<BunCommand>(&raw) {
                        Ok(cmd) => {
                            let cmd_id = cmd.cmd_id.clone();
                            match preprocess(cmd) {
                                Ok(processed) => {
                                    if proxy.send_event(AppEvent::Command(cmd_id, processed)).is_err() {
                                        break; // event loop already exited
                                    }
                                }
                                Err(msg) => emit(&TrayEvent::Error { cmd_id, message: msg }),
                            }
                        }
                        Err(e) => emit(&TrayEvent::Error {
                            cmd_id: None,
                            message: format!("JSON parse error: {}", e),
                        }),
                    }
                }
                Ok(_)  => {} // blank line
                Err(_) => break, // stdin closed / broken pipe
            }
        }
        std::process::exit(0);
    });

    let root_menu = Menu::new();
    let mut registry   = MenuRegistry::new(root_menu.clone());
    let mut states_map: HashMap<String, RgbaBytes> = HashMap::new();

    let mut tray = match TrayIconBuilder::new()
        .with_menu(Box::new(root_menu.clone()))
        .with_tooltip("tray-hook")
        .build()
    {
        Ok(t)  => t,
        Err(e) => {
            emit(&TrayEvent::Error {
                cmd_id: None,
                message: format!("failed to initialise tray icon: {}", e),
            });
            std::process::exit(1);
        }
    };

    // ── Channel watcher threads ───────────────────────────────────────────────
    //
    // Root cause of the Windows click-event delay bug (#1):
    //
    // `ControlFlow::Wait` causes tao's Win32 backend to call `WaitMessage()`,
    // which blocks until a *Win32 message* arrives in the thread queue.
    // `muda` and `tray-icon` deliver their events via crossbeam channels whose
    // writes do NOT post a Win32 message — so the event loop stays asleep even
    // though click data is already sitting in the channel.  The loop only woke
    // up when the user moved the mouse (generating `WM_MOUSEMOVE`), which is
    // exactly the symptom reported on Windows 10.
    //
    // Fix: one dedicated thread per channel.  Each thread blocks on `recv()`
    // and forwards the event through `EventLoopProxy::send_event()`, which
    // posts a `WM_USER`-style message that immediately wakes `WaitMessage()`.
    // This keeps CPU usage at zero while idle and fires events with no delay.

    let proxy_menu = event_loop.create_proxy();
    thread::spawn(move || {
        let menu_rx = MenuEvent::receiver();
        while let Ok(ev) = menu_rx.recv() {
            if proxy_menu.send_event(AppEvent::Menu(ev)).is_err() { break; }
        }
    });

    let proxy_tray = event_loop.create_proxy();
    thread::spawn(move || {
        let tray_rx = TrayIconEvent::receiver();
        while let Ok(ev) = tray_rx.recv() {
            if proxy_tray.send_event(AppEvent::Tray(ev)).is_err() { break; }
        }
    });

    emit(&TrayEvent::Ready);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            // ── Direct tray icon interactions ────────────────────────────
            Event::UserEvent(AppEvent::Tray(ev)) => {
                match ev {
                    TrayIconEvent::Click { button, button_state, .. } => {
                        if button_state == MouseButtonState::Up {
                            let btn = match button {
                                MouseButton::Left  => "left",
                                MouseButton::Right => "right",
                                _                  => return,
                            };
                            emit(&TrayEvent::TrayClick { button: btn.to_string() });
                        }
                    }
                    TrayIconEvent::DoubleClick { .. } => {
                        emit(&TrayEvent::TrayClick { button: "double".to_string() });
                    }
                    _ => {}
                }
            }

            // ── Menu item activations ────────────────────────────────────
            Event::UserEvent(AppEvent::Menu(ev)) => {
                let id = ev.id.0.clone();
                if let Some(checked) = registry.check_state(&id) {
                    emit(&TrayEvent::Check { id, checked });
                } else {
                    emit(&TrayEvent::Click { id });
                }
            }

            // ── Inbound commands from host ───────────────────────────────
            Event::UserEvent(AppEvent::Command(cmd_id, cmd)) => {
                match process(&cmd_id, cmd, &mut registry, &mut tray, &mut states_map) {
                    Ok(Outcome::Ok)        => emit(&TrayEvent::Ack { cmd_id }),
                    Ok(Outcome::Responded) => {} // already emitted its own response
                    Ok(Outcome::Quit)      => *control_flow = ControlFlow::Exit,
                    Err(msg)               => emit(&TrayEvent::Error { cmd_id, message: msg }),
                }
            }

            _ => {}
        }
    });
}
