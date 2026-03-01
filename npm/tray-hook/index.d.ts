import { EventEmitter } from "node:events";

// ─── Menu item options ────────────────────────────────────────────────────────

export interface AddOptions {
  /** Whether the item is clickable. @defaultValue `true` */
  enabled?: boolean;
  /** ID of a submenu to nest this item inside. Omit for root. */
  parent_id?: string;
  /**
   * Path to a PNG/ICO icon shown beside the label.
   * Resolved to an absolute path before sending to the daemon.
   */
  icon?: string;
}

export interface AddCheckOptions {
  /** Initial checked state. @defaultValue `false` */
  checked?: boolean;
  /** Whether the item is clickable. @defaultValue `true` */
  enabled?: boolean;
  /** ID of a submenu to nest this item inside. Omit for root. */
  parent_id?: string;
  /**
   * Path to a PNG/ICO icon shown beside the label.
   * Resolved to an absolute path before sending to the daemon.
   */
  icon?: string;
}

export interface AddSubmenuOptions {
  /** Whether the submenu is accessible. @defaultValue `true` */
  enabled?: boolean;
  /** ID of a parent submenu (for nested submenus). Omit for root. */
  parent_id?: string;
}

export interface AddSeparatorOptions {
  /** ID of a submenu to place the separator inside. Omit for root. */
  parent_id?: string;
}

// ─── setMenu template ─────────────────────────────────────────────────────────

export type MenuItemTemplate =
  | { type: "item";      id: string; title: string;  enabled?: boolean; icon?: string }
  | { type: "check";     id: string; title: string;  enabled?: boolean; checked?: boolean }
  | { type: "separator"; id: string; title?: string }
  | { type: "submenu";   id: string; title: string;  enabled?: boolean; items: MenuItemTemplate[] };

// ─── Events ───────────────────────────────────────────────────────────────────

export interface TrayEvents {
  /** Daemon is up and accepting commands. */
  ready: [];
  /** A regular menu item was activated. */
  click: [id: string];
  /**
   * A check-menu item was activated. `checked` is the new state after the click.
   */
  check: [id: string, checked: boolean];
  /**
   * The tray icon itself was clicked or double-clicked (not a menu item).
   *
   * @remarks
   * On macOS, when a menu is attached, the OS intercepts left-click to open
   * the menu before the event can fire. The `"left"` value is therefore
   * unreliable on macOS and should not be used as a primary interaction
   * trigger in cross-platform apps. `"right"` and `"double"` are reliable
   * on all platforms.
   */
  tray_click: [button: "left" | "right" | "double"];
  /** Daemon process exited. `code` is null if killed by signal. */
  exit: [code: number | null];
  /**
   * Daemon was automatically restarted after a crash and all shadow state
   * has been replayed. Menu, icon, tooltip, and icon states are fully restored.
   */
  restart: [];
  /**
   * Unmatched or protocol-level error from the daemon.
   * @remarks
   * Per Node.js convention, if no `"error"` listener is attached, this event
   * will throw and crash the process. Always attach one.
   */
  error: [err: Error];
}

// ─── Constructor options ──────────────────────────────────────────────────────

export interface TrayOptions {
  /**
   * Whether to automatically restart the daemon after an unexpected crash
   * and replay all tracked state.
   *
   * Protected by a circuit-breaker: if the daemon crashes 5 times within
   * 10 seconds, auto-restart is permanently disabled and an `"error"` event
   * is emitted.
   *
   * @defaultValue `true`
   */
  autoRestart?: boolean;
}

// ─── Tray class ───────────────────────────────────────────────────────────────

export declare class Tray extends EventEmitter {

  /**
   * Create a new Tray instance.
   * @param options - Optional configuration.
   */
  constructor(options?: TrayOptions);

  // ── Lifecycle ────────────────────────────────────────────────────────────

  /**
   * Spawn the Rust daemon and resolve when it signals ready.
   * Idempotent — concurrent calls share the same startup Promise.
   *
   * @returns Resolves when the daemon is live.
   * @throws If the binary cannot be found, permission is denied, or the
   *         daemon crashes before emitting `"ready"`.
   */
  start(): Promise<void>;

  /**
   * Immediately kill the daemon and reject all in-flight commands.
   * The `"exit"` event fires once the OS confirms the process is gone.
   * Call `start()` again to restart.
   */
  destroy(): void;

  /**
   * Permanently disable auto-restart without killing the current daemon.
   */
  disableAutoRestart(): void;

  /**
   * Low-level escape hatch. Sends a raw command and returns a Promise that
   * resolves on ack or rejects on error / timeout (10 s).
   *
   * @param cmd - Raw command object; must include at least `action`.
   * @throws If `start()` has not been called.
   */
  send(cmd: Record<string, unknown>): Promise<unknown>;

  // ── Tray-level controls ──────────────────────────────────────────────────

  /**
   * Set the tray icon from a local image file (PNG, ICO, JPG).
   * Path is resolved to absolute before sending to the daemon.
   *
   * @param iconPath - Relative or absolute path to the icon file.
   * @throws If the file does not exist or the format is unsupported.
   */
  setIcon(iconPath: string): Promise<void>;

  /**
   * Set the tray icon from a base64-encoded PNG string.
   * A `data:image/...;base64,` prefix is stripped automatically.
   *
   * @remarks Not suitable for animation — each call replaces the icon in full.
   * @param base64 - Base64-encoded PNG image data.
   */
  setIconData(base64: string): Promise<void>;

  /**
   * Set the tooltip shown when hovering over the tray icon.
   *
   * @param title - Tooltip text (1–256 chars).
   */
  setTooltip(title: string): Promise<void>;

  /**
   * Set text rendered in the menu bar beside the tray icon.
   *
   * @remarks
   * **macOS only.** Rejects on Windows and Linux via daemon error.
   * Guard with `if (process.platform === 'darwin')`.
   *
   * @param title - Text to display (1–256 chars).
   */
  setTrayTitle(title: string): Promise<void>;

  // ── Menu creation ────────────────────────────────────────────────────────

  /**
   * Add a regular clickable menu item.
   *
   * @param id      - Unique item identifier (1–128 chars, no control chars).
   * @param title   - Label shown in the menu (1–256 chars).
   * @param options - Optional configuration.
   *
   * @throws {Error} If `setMenu()` has previously been called. Update your
   *         template and call `setMenu()` again instead.
   */
  add(id: string, title: string, options?: AddOptions): Promise<void>;

  /**
   * Add a checkable menu item that toggles a checkmark when clicked.
   * Emits `"check"` events — not `"click"`.
   *
   * @param id      - Unique item identifier.
   * @param title   - Label shown in the menu.
   * @param options - Optional configuration.
   *
   * @throws {Error} If `setMenu()` has previously been called.
   */
  addCheck(id: string, title: string, options?: AddCheckOptions): Promise<void>;

  /**
   * Add a submenu — a nested menu that expands on hover.
   * Add child items by passing `parent_id` to other add methods.
   * Maximum nesting depth: 5 levels.
   *
   * @param id      - Unique identifier, also used as `parent_id` for children.
   * @param title   - Label shown in the parent menu.
   * @param options - Optional configuration.
   *
   * @throws {Error} If `setMenu()` has previously been called.
   */
  addSubmenu(id: string, title: string, options?: AddSubmenuOptions): Promise<void>;

  /**
   * Add a horizontal visual divider between menu items.
   * An ID is required so the separator can be removed later.
   *
   * @param id      - Unique identifier for this separator.
   * @param options - Optional configuration.
   *
   * @throws {Error} If `setMenu()` has previously been called.
   */
  addSeparator(id: string, options?: AddSeparatorOptions): Promise<void>;

  // ── Menu mutation ────────────────────────────────────────────────────────

  /**
   * Change the visible label of any item, check item, or submenu.
   * Permitted after `setMenu()`.
   *
   * @param id    - ID of the item to rename.
   * @param title - New label (1–256 chars).
   *
   * @throws If `id` refers to a separator (separators have no title).
   * @throws If `id` is not found in the menu.
   */
  rename(id: string, title: string): Promise<void>;

  /**
   * Enable or disable an item. Disabled items are greyed out and not clickable.
   * Disabling a submenu makes the entire subtree inaccessible.
   * Permitted after `setMenu()`.
   *
   * @param id      - ID of the item to update.
   * @param enabled - New enabled state.
   *
   * @throws If `id` is not found in the menu.
   */
  setEnabled(id: string, enabled: boolean): Promise<void>;

  /**
   * Programmatically set the checked state of a check item without firing
   * a `"check"` event. Use this to sync visual state to app state.
   * Permitted after `setMenu()`.
   *
   * @param id      - ID of the check item.
   * @param checked - New checked state.
   *
   * @throws If `id` does not refer to a check item.
   */
  setChecked(id: string, checked: boolean): Promise<void>;

  /**
   * Flip the checked state of a check item.
   * Permitted after `setMenu()`.
   *
   * @param id - ID of the check item.
   *
   * @throws If `id` does not refer to a check item.
   */
  toggle(id: string): Promise<void>;

  /**
   * Remove an item from the menu and free its ID for reuse.
   * Submenus must have all children removed first.
   *
   * @param id - ID of the item to remove.
   *
   * @throws {Error} If the item is a submenu that still has children.
   * @throws {Error} If `setMenu()` has previously been called. Update your
   *         template and call `setMenu()` again instead.
   */
  remove(id: string): Promise<void>;

  /**
   * Remove all items from the menu atomically.
   * Also clears the `setMenu()` lock — imperative `add*` calls are permitted again.
   */
  clear(): Promise<void>;

  // ── Declarative menu ─────────────────────────────────────────────────────

  /**
   * Replace the entire menu atomically with no visible flicker.
   *
   * After this call, imperative structural mutations (`add`, `addCheck`,
   * `addSubmenu`, `addSeparator`, `remove`) are forbidden and will reject.
   * To modify the menu, update your template array and call `setMenu()` again.
   * `rename`, `setEnabled`, `setChecked`, and `toggle` remain permitted.
   *
   * Icon paths inside the template are resolved to absolute before sending.
   *
   * @param template - Array of `MenuItemTemplate` nodes. Submenus carry
   *                   their own nested `items` array. Max depth: 5 levels.
   */
  setMenu(template: MenuItemTemplate[]): Promise<void>;

  // ── Icon states ───────────────────────────────────────────────────────────

  /**
   * Pre-load a named set of tray icons. All icon files are decoded immediately
   * on the daemon's I/O thread so subsequent `setState()` calls are instantaneous
   * with no file I/O on the GUI thread.
   *
   * @param states - Map of `{ stateName: iconFilePath }`.
   *
   * @throws If any icon file cannot be found or decoded.
   */
  defineStates(states: Record<string, string>): Promise<void>;

  /**
   * Switch the tray icon to a pre-loaded named state instantly.
   *
   * @param stateName - Name of a state previously registered via `defineStates()`.
   *
   * @throws If `stateName` was not previously defined via `defineStates()`.
   */
  setState(stateName: string): Promise<void>;

  // ── Autostart ─────────────────────────────────────────────────────────────

  /**
   * Register or unregister the application as a system startup entry.
   *
   * @param appId     - A stable unique identifier for the app
   *                    (e.g. `"com.example.myapp"`).
   * @param execPath  - Full command to execute on startup
   *                    (e.g. `"/usr/bin/node /app/server.js"`).
   *                    On macOS, space-separated tokens are split into
   *                    individual `ProgramArguments` entries; quoted
   *                    arguments with embedded spaces are respected.
   * @param enabled   - `true` to register, `false` to unregister.
   *
   * @remarks
   * - **macOS**: writes/deletes a LaunchAgent plist at
   *   `~/Library/LaunchAgents/<appId>.plist`. No daemon involvement.
   * - **Linux**: writes/deletes a `.desktop` file at
   *   `~/.config/autostart/<appId>.desktop`. No daemon involvement.
   * - **Windows**: writes/deletes a registry value under
   *   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` via the daemon.
   */
  setAutostart(appId: string, execPath: string, enabled: boolean): Promise<void>;

  /**
   * Check whether the application is currently registered as a startup entry.
   *
   * @param appId - The same identifier passed to `setAutostart()`.
   * @returns `true` if registered, `false` otherwise.
   *
   * @remarks
   * - **macOS / Linux**: resolves synchronously from the local filesystem.
   *   No daemon involvement.
   * - **Windows**: queries the Windows Registry via the daemon.
   */
  getAutostart(appId: string): Promise<boolean>;

  // ── Lifecycle ────────────────────────────────────────────────────────────

  /**
   * Gracefully ask the daemon to exit.
   * The daemon removes the tray icon from the OS tray and exits with code 0.
   * Resolves immediately — the command is fire-and-forget.
   */
  quit(): Promise<void>;

  // ── Typed event overloads ────────────────────────────────────────────────

  on<K extends keyof TrayEvents>(event: K, listener: (...args: TrayEvents[K]) => void): this;
  once<K extends keyof TrayEvents>(event: K, listener: (...args: TrayEvents[K]) => void): this;
  off<K extends keyof TrayEvents>(event: K, listener: (...args: TrayEvents[K]) => void): this;
  emit<K extends keyof TrayEvents>(event: K, ...args: TrayEvents[K]): boolean;
}

/**
 * Convenience factory. Equivalent to `new Tray(options)`.
 *
 * @param options - Optional configuration passed to the `Tray` constructor.
 */
export declare function createTray(options?: TrayOptions): Tray;
