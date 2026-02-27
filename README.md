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

await tray.add('open',  'Open App');
await tray.add('quit',  'Quit');

tray.on('click', (id) => {
  if (id === 'open')  openApp();
  if (id === 'quit')  tray.quit();
});
```

That's it. A tray icon with a menu, driven entirely from JS, with no native build step.

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

tray-hook ships a pre-compiled Rust binary for each platform. When you call `tray.start()`, it spawns that binary as a child process. All commands flow as newline-delimited JSON over stdin; all events (clicks, errors, acks) flow back over stdout. The daemon manages the OS event loop and native menu objects so your JS never has to.

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

## API Reference

### `createTray()`

Factory function. Returns a new `Tray` instance. Equivalent to `new Tray()`.

```javascript
import { createTray } from 'tray-hook';
const tray = createTray();
```

---

### Class: `Tray extends EventEmitter`

#### `tray.start() → Promise<void>`

Spawns the Rust daemon and resolves when it's ready to accept commands.

- **Idempotent:** Safe to call multiple times. Concurrent calls all receive the same Promise and share the same startup.
- **Rejects** if the binary fails to spawn, is not found, or exits before signalling ready.
- All commands sent before `start()` resolves are queued and flushed automatically.

```javascript
await tray.start();
// Daemon is ready — all subsequent commands execute immediately
```

> **Always `await` this** before calling any other method. Calling methods before `start()` is safe — they queue — but errors during startup will reject those queued commands.

---

#### `tray.destroy() → void`

Immediately kills the daemon and rejects all in-flight commands.

- Synchronous. Does not wait for the process to exit.
- Clears the command queue.
- Emits `"exit"` once the OS confirms the process is gone.
- After `destroy()`, you can call `start()` again to restart.

```javascript
// Force teardown — use for unrecoverable error states
tray.destroy();
```

> For graceful shutdown, prefer `tray.quit()` which lets the daemon clean up the tray icon before exiting.

---

#### `tray.send(cmd) → Promise<void>`

Low-level escape hatch. Sends a raw command object to the daemon and returns a Promise that resolves on ack or rejects on error or timeout.

All higher-level methods call this internally. Use it only if you need to send a command not yet covered by the typed API.

```javascript
await tray.send({ action: 'set_tooltip', title: 'Hello' });
```

Every command times out after **10 seconds** if no ack is received, rejecting with a descriptive error.

---

### Menu Creation

All creation methods share a common rule: **IDs must be unique.** Registering the same ID twice rejects with an error.

---

#### `tray.add(id, title, options?) → Promise<void>`

Adds a regular clickable menu item.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `string` | ✅ | Unique identifier. Used to reference this item in future calls and in click events. |
| `title` | `string` | ✅ | Label shown in the menu. |
| `options.enabled` | `boolean` | — | Whether the item is clickable. Default: `true`. |
| `options.parent_id` | `string` | — | ID of a submenu to nest this item inside. Omit for root level. |

```javascript
await tray.add('open',  'Open Window');
await tray.add('about', 'About',  { enabled: false });
await tray.add('sub-item', 'Sub Item', { parent_id: 'my-submenu' });
```

---

#### `tray.addCheck(id, title, options?) → Promise<void>`

Adds a checkable menu item that toggles a visible checkmark when clicked.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `string` | ✅ | Unique identifier. |
| `title` | `string` | ✅ | Label shown in the menu. |
| `options.checked` | `boolean` | — | Initial checked state. Default: `false`. |
| `options.enabled` | `boolean` | — | Whether the item is clickable. Default: `true`. |
| `options.parent_id` | `string` | — | ID of a submenu to nest this item inside. |

```javascript
await tray.addCheck('dark-mode', 'Dark Mode', { checked: true });
await tray.addCheck('notifications', 'Enable Notifications');
```

When clicked, emits a `"check"` event (not `"click"`) with the new boolean state. See [Events](#events).

---

#### `tray.addSubmenu(id, title, options?) → Promise<void>`

Adds a submenu — a nested menu that expands on hover. Items are added into it by passing `parent_id` to other add methods.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `string` | ✅ | Unique identifier. Used as `parent_id` for child items. |
| `title` | `string` | ✅ | Label shown in the parent menu. |
| `options.enabled` | `boolean` | — | Whether the submenu is accessible. Default: `true`. |
| `options.parent_id` | `string` | — | ID of a parent submenu (for nesting submenus). |

```javascript
await tray.addSubmenu('settings', 'Settings');
await tray.add('theme',    'Change Theme',    { parent_id: 'settings' });
await tray.add('language', 'Change Language', { parent_id: 'settings' });
```

Maximum nesting depth is **5 levels**.

---

#### `tray.addSeparator(id, options?) → Promise<void>`

Adds a horizontal visual divider line between menu items.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `id` | `string` | ✅ | Unique identifier. Required even for separators so they can be removed later. |
| `options.parent_id` | `string` | — | ID of a submenu to place the separator inside. |

```javascript
await tray.add('open', 'Open');
await tray.addSeparator('sep-1');
await tray.add('quit', 'Quit');
```

---

### Menu Mutation

Items can be modified at any time after creation. Changes are reflected immediately in the live menu.

---

#### `tray.rename(id, title) → Promise<void>`

Changes the visible label of any item, check item, or submenu.

```javascript
await tray.rename('sync', 'Syncing...');
// later
await tray.rename('sync', 'Sync Complete ✓');
```

> Calling `rename` on a separator rejects — separators have no title.

---

#### `tray.setEnabled(id, enabled) → Promise<void>`

Enables or disables an item. Disabled items are visible but greyed out and not clickable.

```javascript
await tray.setEnabled('export', false); // grey out
await tray.setEnabled('export', true);  // restore
```

> Works on regular items, check items, and submenus (disabling a submenu makes it inaccessible).

---

#### `tray.setChecked(id, checked) → Promise<void>`

Programmatically sets the checked state of a check item without triggering a `"check"` event.

```javascript
// Sync visual state to app state without a user interaction
await tray.setChecked('dark-mode', app.isDarkMode());
```

> Rejects if `id` refers to a non-check item.

---

#### `tray.toggle(id) → Promise<void>`

Flips the checked state of a check item. Equivalent to `setChecked(id, !current)`.

```javascript
await tray.toggle('mute');
```

> Rejects if `id` refers to a non-check item.

---

#### `tray.remove(id) → Promise<void>`

Removes an item from the menu entirely and frees its ID for reuse.

```javascript
await tray.remove('sep-1');
await tray.remove('old-item');
```

**Constraints:**
- Removing a submenu that still has children rejects with an error. Remove all children first, or use `tray.clear()`.
- After removal, the ID is free and can be registered again with a new add call.

```javascript
// Correct teardown order for a submenu
await tray.remove('sub-child-1');
await tray.remove('sub-child-2');
await tray.remove('my-submenu'); // now safe
```

---

#### `tray.clear() → Promise<void>`

Removes all items from the menu at once. Resets the menu to empty.

```javascript
await tray.clear();
// Menu is now empty — rebuild from scratch
await tray.add('quit', 'Quit');
```

> Useful for dynamically rebuilding a context menu in response to app state changes.

---

### Tray-Level Controls

---

#### `tray.setIcon(iconPath) → Promise<void>`

Sets the tray icon from an image file.

| Format | Windows | macOS | Linux |
|--------|---------|-------|-------|
| PNG | ✅ | ✅ | ✅ |
| ICO | ✅ | ✅ | ✅ |
| JPG | ✅ | ✅ | ✅ |

```javascript
await tray.setIcon('./icons/tray.png');
await tray.setIcon('./icons/tray-active.png'); // swap dynamically
```

**Recommendations:**
- Use **PNG** for best cross-platform results
- Provide a **template image** (black + transparent) on macOS for automatic dark/light mode support
- Recommended sizes: `16×16`, `32×32`, or `22×22` (Linux)
- Path is resolved to absolute automatically

---

#### `tray.setTooltip(title) → Promise<void>`

Sets the tooltip shown when the user hovers over the tray icon.

```javascript
await tray.setTooltip('My App — Connected');
await tray.setTooltip('My App — 3 notifications');
```

---

#### `tray.setTrayTitle(title) → Promise<void>`

**macOS only.** Sets text rendered directly in the menu bar beside the icon.

```javascript
await tray.setTrayTitle('●'); // status dot
await tray.setTrayTitle('3'); // notification count
await tray.setTrayTitle('');  // clear it
```

> On Windows and Linux this rejects via a daemon error. Wrap in try/catch or guard with a platform check if your code runs cross-platform:
> ```javascript
> if (process.platform === 'darwin') await tray.setTrayTitle('●');
> ```

---

### Lifecycle

---

#### `tray.quit() → Promise<void>`

Sends a graceful shutdown command to the daemon. The daemon removes the tray icon from the OS, then exits cleanly.

```javascript
tray.on('click', async (id) => {
  if (id === 'quit') {
    await tray.quit();
    process.exit(0);
  }
});
```

> After `quit()` resolves, the daemon process is gone. The `"exit"` event fires shortly after. Do not call any further tray methods after this.

---

### Events

`Tray` extends Node's `EventEmitter`. Attach listeners with `.on()`, `.once()`, or `.off()`.

---

#### `"ready"`

Emitted once when the daemon is up and the tray icon has been registered with the OS. Equivalent to the Promise returned by `start()` resolving.

```javascript
tray.on('ready', () => console.log('Tray is live'));
```

---

#### `"click"` · `(id: string)`

Emitted when a **regular** menu item is activated.

```javascript
tray.on('click', (id) => {
  switch (id) {
    case 'open':  openMainWindow(); break;
    case 'quit':  tray.quit();      break;
  }
});
```

> Check items emit `"check"`, not `"click"`. See below.

---

#### `"check"` · `(id: string, checked: boolean)`

Emitted when a **check item** is activated. `checked` is the **new** state after the click.

```javascript
tray.on('check', (id, checked) => {
  if (id === 'dark-mode') applyTheme(checked ? 'dark' : 'light');
  if (id === 'autostart') setAutostart(checked);
});
```

---

#### `"exit"` · `(code: number | null)`

Emitted when the daemon process exits, whether gracefully (via `quit()`) or unexpectedly. `code` is `null` if the process was killed by a signal.

```javascript
tray.on('exit', (code) => {
  if (code !== 0) console.error(`Tray daemon crashed (code ${code})`);
});
```

---

#### `"error"` · `(err: Error)`

Emitted for unmatched or protocol-level errors from the daemon — errors that could not be correlated to a specific command call.

> **Important:** Per Node.js convention, if no `"error"` listener is attached and this event fires, Node will throw and crash your process. Always attach one.

```javascript
tray.on('error', (err) => {
  console.error('[tray-hook error]', err.message);
});
```

---

## Patterns & Recipes

### Dynamic Menu (Live Updates)

```javascript
const tray = createTray();
await tray.start();
await tray.setIcon('./icon.png');

await tray.add('status', 'Status: Connecting...', { enabled: false });
await tray.addSeparator('sep');
await tray.add('quit', 'Quit');

// Update label as connection state changes
app.on('connected',    () => tray.rename('status', 'Status: Connected ✓'));
app.on('disconnected', () => tray.rename('status', 'Status: Offline'));
```

---

### Rebuilding the Menu on Demand

```javascript
async function buildMenu(items) {
  await tray.clear();
  for (const item of items) {
    await tray.add(item.id, item.label);
  }
  await tray.addSeparator('sep');
  await tray.add('quit', 'Quit');
}

// Rebuild whenever your data changes
store.on('change', () => buildMenu(store.getRecentItems()));
```

---

### Nested Submenus

```javascript
await tray.addSubmenu('view', 'View');

await tray.addSubmenu('zoom', 'Zoom', { parent_id: 'view' });
await tray.add('zoom-in',  'Zoom In',  { parent_id: 'zoom' });
await tray.add('zoom-out', 'Zoom Out', { parent_id: 'zoom' });

await tray.addSeparator('view-sep', { parent_id: 'view' });
await tray.addCheck('fullscreen', 'Full Screen', { parent_id: 'view' });
```

---

### Notification Badge (macOS)

```javascript
let unread = 0;

function setUnread(n) {
  unread = n;
  tray.setTrayTitle(n > 0 ? String(n) : '');
  tray.setTooltip(n > 0 ? `${n} unread messages` : 'My App');
}

app.on('message', () => setUnread(unread + 1));
app.on('opened',  () => setUnread(0));
```

---

### Graceful Shutdown

```javascript
tray.on('click', async (id) => {
  if (id !== 'quit') return;
  await tray.quit();   // remove icon from OS tray
  process.exit(0);
});

// Also handle OS-level signals
process.on('SIGTERM', async () => {
  await tray.quit();
  process.exit(0);
});

// If the daemon crashes unexpectedly, don't hang
tray.on('exit', (code) => {
  if (code !== 0) process.exit(1);
});
```

---

### Error Handling (Production)

```javascript
const tray = createTray();

tray.on('error', (err) => {
  logger.error('tray-hook:', err.message);
});

try {
  await tray.start();
} catch (err) {
  // Binary not found, permission denied, unsupported platform, etc.
  console.error('Failed to start tray:', err.message);
  process.exit(1);
}

// Individual command errors
try {
  await tray.add('item', 'My Item');
} catch (err) {
  // e.g. "id 'item' already exists", validation errors, timeout
  console.warn('Could not add menu item:', err.message);
}
```

---

## Troubleshooting

### Binary Not Found

```
tray-hook: could not find native binary. Is '@phtdacosta/tray-hook-darwin-arm64' installed?
```

**Fix:** The platform-specific package wasn't installed. This usually means npm skipped it because `os`/`cpu` fields didn't match, or you're running under Rosetta.

```bash
npm install @phtdacosta/tray-hook-darwin-arm64 --ignore-platform
```

---

### Tray Icon Doesn't Appear (Linux)

No error is thrown, but the icon is invisible.

**Fix:** Your desktop environment doesn't have a system tray host. Install the AppIndicator extension for GNOME:

```bash
# Ubuntu/Debian
sudo apt install gnome-shell-extension-appindicator
# then enable in GNOME Extensions
```

---

### `setTrayTitle` Throws on Windows/Linux

```
Error: [daemon] set_tray_title is only supported on macOS
```

**Expected.** This feature is macOS-only. Guard with a platform check:

```javascript
if (process.platform === 'darwin') {
  await tray.setTrayTitle('●');
}
```

---

### Command Times Out

```
Error: tray-hook: command 'add' (cmd_id=3) timed out after 10000ms
```

The daemon didn't respond within 10 seconds. This usually means it crashed or was killed externally. Check the `"exit"` event and restart with `tray.start()`.

---

### `remove` Rejects on a Submenu

```
Error: 'my-submenu' still has children — remove them first
```

**Fix:** Remove all child items before removing their parent submenu. Or use `tray.clear()` to wipe everything at once.

---

### Multiple `start()` Calls

```javascript
await tray.start();
await tray.start(); // ← returns the same resolved Promise — safe, does nothing
```

`start()` is idempotent by design. The second call resolves immediately.

---

## Constraints & Limits

| Constraint | Value | Reason |
|-----------|-------|--------|
| Max ID length | 128 chars | Prevents unbounded memory use in the daemon |
| Max title length | 256 chars | OS menu label limits |
| Max nesting depth | 5 levels | Platform consistency (Windows is the limiting factor) |
| Command timeout | 10 seconds | Prevents hanging Promises on daemon crash |
| ID characters | No control chars (`\x00–\x1f`) | JSON safety |

---

## License

MIT

---

## Credits

Created by [@phteocos](https://x.com/phteocos). Built for [**THYPRESS**](https://thypress.org) — zero-config static site generator.