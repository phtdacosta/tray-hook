# 🪝 tray-hook

**Cross-platform native system tray for Bun and Node.js**

![npm version](https://img.shields.io/npm/v/tray-hook.svg)
![npm downloads (monthly)](https://img.shields.io/npm/dm/tray-hook.svg)
![npm downloads (total)](https://img.shields.io/npm/dt/tray-hook.svg)
![license](https://img.shields.io/npm/l/tray-hook.svg)
![GitHub stars](https://img.shields.io/github/stars/phtdacosta/tray-hook?style=social)
![GitHub issues](https://img.shields.io/github/issues/phtdacosta/tray-hook)
![GitHub last commit](https://img.shields.io/github/last-commit/phtdacosta/tray-hook)

Add a system tray icon with a fully dynamic menu to any Bun or Node.js app — no native compilation, no Electron, no framework. A lean Rust daemon handles the OS integration; you drive it entirely from JavaScript.

---

## Quick Start

```bash
npm install tray-hook
```

```javascript
import { createTray } from 'tray-hook';

const tray = createTray();
await tray.start();

await tray.setIcon('./icon.png');
await tray.setTooltip('My App');

await tray.add('open', 'Open App');
await tray.add('quit', 'Quit');

tray.on('click', (id) => {
  if (id === 'open') openApp();
  if (id === 'quit') tray.quit();
});
```

---

## How It Works

```
Your JS code
    │
    │  JSON over stdin/stdout
    ▼
tray-hook daemon (Rust)
    │
    │  OS native APIs
    ▼
System Tray (Windows/macOS/Linux)
```

tray-hook ships a pre-compiled Rust binary for each platform. When you call `tray.start()`, it spawns that binary as a child process. All commands flow as newline-delimited JSON over stdin; all events flow back over stdout. The daemon manages the OS event loop and native menu objects so your JS never has to.

**Why Rust?**
- Native OS tray APIs require a GUI event loop that must own the main thread — impossible in Node/Bun without native addons
- No N-API ABI compatibility headaches across runtime versions
- Pre-compiled binaries mean zero build step for users

**Trade-off:** The daemon is a separate process (~5MB). IPC adds ~1ms of latency per command, which is imperceptible for menu operations.

---

## Platform Support

| Platform | Architecture | Status |
|----------|-------------|--------|
| **Windows** | x64 | ✅ Supported |
| **Windows** | arm64 | ✅ Supported |
| **macOS** | x64 (Intel) | ✅ Supported |
| **macOS** | arm64 (Apple Silicon) | ✅ Supported |
| **Linux** | x64 | ✅ Supported |
| **Linux** | arm64 | ✅ Supported |

> **Linux note:** Requires a system tray host. GNOME users need the [AppIndicator extension](https://extensions.gnome.org/extension/615/appindicator-support/). KDE, XFCE, and most other DEs work out of the box.

---

## Installation

The main package auto-selects and installs only the binary for your current platform via `optionalDependencies`. You never download binaries for platforms you don't use.

```bash
# npm
npm install tray-hook

# Bun
bun add tray-hook
```

---

## What's New in v1.1.0

- **Tray icon click events** — detect left, right, and double-clicks directly on the tray icon via the new `tray_click` event
- **Declarative menus** — `setMenu(template)` replaces the entire menu atomically with no flicker, using a typed template tree
- **Named icon states** — `defineStates()` / `setState()` for instant, pre-loaded icon switching with no I/O at switch time
- **Base64 icon support** — `setIconData()` sets the tray icon from a dynamically generated base64 PNG string
- **Autostart API** — `setAutostart()` / `getAutostart()` for cross-platform system startup registration
- **Auto-restart with state replay** — the daemon automatically respawns after an unexpected crash and replays the full menu, icon, tooltip, and state — your tray reappears without user intervention
- **Crash circuit-breaker** — auto-restart is permanently disabled after 5 crashes within 10 seconds to prevent CPU-pegging infinite loops

---

## API Reference

### `createTray(options?)`

Factory function. Returns a new `Tray` instance.

```javascript
import { createTray } from 'tray-hook';

const tray = createTray({ autoRestart: true }); // autoRestart defaults to true
```

---

### Class: `Tray extends EventEmitter`

#### `tray.start() → Promise<void>`

Spawns the Rust daemon and resolves when it's ready to accept commands. Idempotent — concurrent calls share the same Promise.

```javascript
await tray.start();
```

---

#### `tray.destroy() → void`

Immediately kills the daemon and rejects all in-flight commands. For graceful shutdown, prefer `tray.quit()`.

---

#### `tray.disableAutoRestart() → void`

Permanently disables auto-restart without killing the current daemon.

---

#### `tray.send(cmd) → Promise<unknown>`

Low-level escape hatch. All higher-level methods call this internally. Commands time out after **10 seconds**.

---

### Tray-Level Controls

---

#### `tray.setIcon(iconPath) → Promise<void>`

Sets the tray icon from a local image file. Path is resolved to absolute automatically.

| Format | Windows | macOS | Linux |
|--------|---------|-------|-------|
| PNG | ✅ | ✅ | ✅ |
| ICO | ✅ | ✅ | ✅ |
| JPG | ✅ | ✅ | ✅ |

```javascript
await tray.setIcon('./icons/tray.png');
```

---

#### `tray.setIconData(base64) → Promise<void>`

Sets the tray icon from a base64-encoded PNG string. A `data:image/...;base64,` prefix is stripped automatically. For static dynamically-generated icons only — not suitable for animation.

```javascript
const png = generateIconAsBase64();
await tray.setIconData(png);
// or with data URL prefix:
await tray.setIconData('data:image/png;base64,...');
```

---

#### `tray.setTooltip(title) → Promise<void>`

Sets the tooltip shown on hover.

```javascript
await tray.setTooltip('My App — 3 notifications');
```

---

#### `tray.setTrayTitle(title) → Promise<void>`

**macOS only.** Sets text beside the tray icon in the menu bar. Rejects on Windows and Linux.

```javascript
if (process.platform === 'darwin') await tray.setTrayTitle('●');
```

---

### Named Icon States

Pre-load multiple icons by name so switching between them is instantaneous with no disk I/O at switch time. All decoding happens eagerly when `defineStates()` is called.

#### `tray.defineStates(states) → Promise<void>`

```javascript
await tray.defineStates({
  idle:       './icons/idle.png',
  active:     './icons/active.png',
  error:      './icons/error.png',
});
```

#### `tray.setState(stateName) → Promise<void>`

```javascript
await tray.setState('active');
// later...
await tray.setState('error');
```

---

### Menu Creation

All IDs must be unique. Registering the same ID twice rejects with an error.

> **Note:** Once `setMenu()` has been called, `add`, `addCheck`, `addSubmenu`, `addSeparator`, and `remove` are forbidden and will reject. Update your template and call `setMenu()` again. `rename`, `setEnabled`, `setChecked`, and `toggle` remain permitted after `setMenu()`.

---

#### `tray.add(id, title, options?) → Promise<void>`

Adds a regular clickable menu item.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `id` | `string` | — | Unique identifier |
| `title` | `string` | — | Label shown in menu |
| `options.enabled` | `boolean` | `true` | Whether item is clickable |
| `options.parent_id` | `string` | — | ID of a submenu to nest inside |

```javascript
await tray.add('open',  'Open Window');
await tray.add('about', 'About', { enabled: false });
await tray.add('sub-item', 'Sub Item', { parent_id: 'my-submenu' });
```

---

#### `tray.addCheck(id, title, options?) → Promise<void>`

Adds a checkable menu item. Emits `"check"` events (not `"click"`).

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `id` | `string` | — | Unique identifier |
| `title` | `string` | — | Label shown in menu |
| `options.checked` | `boolean` | `false` | Initial checked state |
| `options.enabled` | `boolean` | `true` | Whether item is clickable |
| `options.parent_id` | `string` | — | ID of a submenu to nest inside |

```javascript
await tray.addCheck('dark-mode', 'Dark Mode', { checked: true });
```

---

#### `tray.addSubmenu(id, title, options?) → Promise<void>`

Adds a submenu that expands on hover. Maximum nesting depth: 5 levels.

```javascript
await tray.addSubmenu('settings', 'Settings');
await tray.add('theme',    'Change Theme',    { parent_id: 'settings' });
await tray.add('language', 'Change Language', { parent_id: 'settings' });
```

---

#### `tray.addSeparator(id, options?) → Promise<void>`

Adds a horizontal divider. IDs are required so separators can be removed later.

```javascript
await tray.add('open', 'Open');
await tray.addSeparator('sep-1');
await tray.add('quit', 'Quit');
```

---

### Declarative Menus

#### `tray.setMenu(template) → Promise<void>`

Replaces the entire menu atomically with no visible flicker. Accepts a typed tree of `MenuItemTemplate` nodes.

```javascript
await tray.setMenu([
  { type: 'item',      id: 'open',  title: 'Open' },
  { type: 'check',     id: 'dark',  title: 'Dark Mode', checked: false },
  { type: 'separator', id: 'sep-1' },
  { type: 'submenu',   id: 'more',  title: 'More', items: [
    { type: 'item', id: 'about', title: 'About' }
  ]},
  { type: 'item', id: 'quit', title: 'Quit' }
]);
```

**Template node types:**

| `type` | Required fields | Optional fields |
|--------|----------------|-----------------|
| `"item"` | `id`, `title` | `enabled`, `icon` |
| `"check"` | `id`, `title` | `enabled`, `checked` |
| `"separator"` | `id` | — |
| `"submenu"` | `id`, `title`, `items` | `enabled` |

To update the menu after calling `setMenu()`, mutate your template and call `setMenu()` again. Property mutations (`rename`, `setEnabled`, `setChecked`, `toggle`) are still allowed without a full rebuild.

---

### Menu Mutation

All mutation methods work regardless of whether the menu was built imperatively or via `setMenu()`.

#### `tray.rename(id, title) → Promise<void>`

```javascript
await tray.rename('sync', 'Syncing...');
```

#### `tray.setEnabled(id, enabled) → Promise<void>`

```javascript
await tray.setEnabled('export', false);
```

#### `tray.setChecked(id, checked) → Promise<void>`

```javascript
await tray.setChecked('dark-mode', app.isDarkMode());
```

#### `tray.toggle(id) → Promise<void>`

```javascript
await tray.toggle('mute');
```

#### `tray.remove(id) → Promise<void>`

Remove a submenu's children before removing the submenu itself.

```javascript
await tray.remove('old-item');
```

#### `tray.clear() → Promise<void>`

Removes all items. Also clears the `setMenu()` lock so imperative adds are permitted again.

```javascript
await tray.clear();
```

---

### Autostart

Register or unregister your app as a system startup entry.

#### `tray.setAutostart(appId, execPath, enabled) → Promise<void>`
#### `tray.getAutostart(appId) → Promise<boolean>`

| Platform | Mechanism |
|----------|-----------|
| **macOS** | LaunchAgent plist at `~/Library/LaunchAgents/<appId>.plist` |
| **Linux** | `.desktop` file at `~/.config/autostart/<appId>.desktop` |
| **Windows** | Registry value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` |

```javascript
await tray.setAutostart('com.example.myapp', '/usr/bin/node /app/server.js', true);

const enabled = await tray.getAutostart('com.example.myapp');
console.log('Starts on boot:', enabled);

// Unregister
await tray.setAutostart('com.example.myapp', '/usr/bin/node /app/server.js', false);
```

> **macOS note:** `execPath` is split on whitespace into separate `ProgramArguments` entries. Quoted arguments with embedded spaces are respected. Passing a single unsplit string causes launchd to fail silently — tray-hook handles the splitting for you automatically.

---

### Events

#### `"ready"`
Emitted once when the daemon is live.

#### `"click"` · `(id: string)`
Emitted when a regular menu item is activated.

```javascript
tray.on('click', (id) => {
  if (id === 'quit') tray.quit();
});
```

#### `"check"` · `(id: string, checked: boolean)`
Emitted when a check item is activated. `checked` is the new state.

```javascript
tray.on('check', (id, checked) => {
  if (id === 'dark-mode') applyTheme(checked ? 'dark' : 'light');
});
```

#### `"tray_click"` · `(button: "left" | "right" | "double")`
Emitted when the user interacts directly with the tray icon itself (not a menu item).

```javascript
tray.on('tray_click', (button) => {
  if (button === 'double') openMainWindow();
  if (button === 'right')  showContextInfo();
});
```

> **macOS caveat:** When a menu is attached to the tray icon, the OS intercepts left-click to open the menu before the event can fire. The `"left"` value is unreliable on macOS. Use `"right"` or `"double"` for cross-platform interactions.

#### `"exit"` · `(code: number | null)`
Emitted when the daemon process exits.

#### `"restart"`
Emitted after the daemon has been automatically restarted and all shadow state has been replayed. The tray icon, menu, tooltip, and icon states are fully restored.

```javascript
tray.on('restart', () => console.log('Tray daemon recovered'));
```

#### `"error"` · `(err: Error)`
Emitted for unmatched or protocol-level errors.

> **Important:** If no `"error"` listener is attached, Node.js will throw and crash your process. Always attach one.

```javascript
tray.on('error', (err) => console.error('[tray-hook]', err.message));
```

---

### Auto-Restart & Crash Recovery

By default, if the daemon crashes unexpectedly, tray-hook automatically respawns it and replays all tracked state — icon, tooltip, menu structure, check states, named icon states — so the tray reappears without any user intervention.

```javascript
const tray = createTray({ autoRestart: true }); // default

tray.on('restart', () => console.log('Tray recovered after crash'));
tray.on('error',   (err) => console.error('Tray error:', err.message));
```

**Circuit-breaker:** If the daemon crashes 5 times within 10 seconds, auto-restart is permanently disabled and a fatal `"error"` event is emitted. This prevents a bad payload from pegging the CPU in an infinite crash loop.

```javascript
// Disable auto-restart entirely
const tray = createTray({ autoRestart: false });

// Or disable it later at runtime
tray.disableAutoRestart();
```

---

## Patterns & Recipes

### Dynamic Status Icon

```javascript
await tray.defineStates({
  idle:       './icons/idle.png',
  syncing:    './icons/syncing.png',
  error:      './icons/error.png',
});

await tray.setState('idle');

app.on('sync:start', () => tray.setState('syncing'));
app.on('sync:done',  () => tray.setState('idle'));
app.on('sync:error', () => tray.setState('error'));
```

---

### Declarative Menu with Live Mutations

```javascript
await tray.setMenu([
  { type: 'item',  id: 'status', title: 'Status: Stopped', enabled: false },
  { type: 'item',  id: 'toggle', title: 'Start Server' },
  { type: 'separator', id: 'sep' },
  { type: 'check', id: 'autostart', title: 'Auto-Start on Boot' },
  { type: 'separator', id: 'sep2' },
  { type: 'item',  id: 'quit',   title: 'Quit' },
]);

let running = false;

tray.on('click', async (id) => {
  if (id === 'toggle') {
    running = !running;
    // rename/setEnabled are allowed after setMenu()
    await tray.rename('status', running ? 'Status: Running ✓' : 'Status: Stopped');
    await tray.rename('toggle', running ? 'Stop Server' : 'Start Server');
  }
  if (id === 'quit') { await tray.quit(); process.exit(0); }
});
```

---

### Tray Icon Interactions

```javascript
tray.on('tray_click', (button) => {
  if (button === 'double') openMainWindow();

  // Right-click: safe to use on all platforms
  if (button === 'right') showQuickActions();

  // Left-click: unreliable on macOS when a menu is attached
  if (button === 'left' && process.platform !== 'darwin') toggleWindow();
});
```

---

### Graceful Shutdown

```javascript
tray.on('click', async (id) => {
  if (id !== 'quit') return;
  await tray.quit();
  process.exit(0);
});

process.on('SIGTERM', async () => { await tray.quit(); process.exit(0); });

tray.on('exit', (code) => { if (code !== 0) process.exit(1); });
```

---

## Troubleshooting

### Binary Not Found

```
tray-hook: could not find native binary. Is '@phtdacosta/tray-hook-darwin-arm64' installed?
```

**Fix:** The platform package wasn't installed. Force-install it:

```bash
npm install @phtdacosta/tray-hook-darwin-arm64 --ignore-platform
```

---

### Tray Icon Doesn't Appear (Linux)

No error is thrown but the icon is invisible.

**Fix:** Install the AppIndicator extension for GNOME:

```bash
sudo apt install gnome-shell-extension-appindicator
```

---

### `setTrayTitle` Rejects on Windows/Linux

```
Error: [daemon] set_tray_title is only supported on macOS
```

Expected. Guard with a platform check:

```javascript
if (process.platform === 'darwin') await tray.setTrayTitle('●');
```

---

### Imperative Add Rejects After `setMenu()`

```
Error: tray-hook: cannot mutate menu structure imperatively after setMenu()
```

**Fix:** Update your template array and call `setMenu()` again. `rename`, `setEnabled`, `setChecked`, and `toggle` are still allowed.

---

### Fatal Crash Loop Error

```
Error: tray-hook: daemon crashed 5 times within 10000ms — auto-restart disabled
```

The daemon crashed repeatedly with the same payload. Check your icon paths, menu templates, and command arguments for invalid values. After fixing, call `tray.start()` manually to restart.

---

### Command Times Out

```
Error: tray-hook: command 'add' (cmd_id=3) timed out after 10000ms
```

The daemon didn't respond within 10 seconds — likely crashed or was killed externally. Check the `"exit"` event; auto-restart will handle recovery if enabled.

---

### `remove` Rejects on a Submenu

```
Error: 'my-submenu' still has children — remove them first
```

**Fix:** Remove all child items before the parent, or use `tray.clear()`.

---

## Constraints & Limits

| Constraint | Value |
|-----------|-------|
| Max ID length | 128 chars |
| Max title length | 256 chars |
| Max nesting depth | 5 levels |
| Command timeout | 10 seconds |
| Auto-restart max crashes | 5 per 10 seconds |
| ID characters | No control chars (`\x00–\x1f`) |

---

## License

MIT

---

## Credits

Created by [@phteocos](https://x.com/phteocos). Built for [**THYPRESS**](https://thypress.org) — zero-config static site generator.
