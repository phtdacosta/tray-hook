/**
 * tray-hook v1.1.0
 *
 * Events emitted on Tray instance:
 *   "ready"                   — daemon is up and accepting commands
 *   "click"      (id)         — a regular menu item was activated
 *   "check"      (id, bool)   — a check item was toggled; bool is new state
 *   "tray_click" (button)     — direct tray icon interaction: "left"|"right"|"double"
 *   "exit"       (code)       — daemon process exited
 *   "restart"                 — daemon was auto-restarted and shadow state replayed
 *   "error"      (Error)      — unmatched / protocol-level error
 */

import { spawn }                                       from "node:child_process";
import { existsSync, mkdirSync, unlinkSync,
         writeFileSync }                               from "node:fs";
import { createRequire }                               from "node:module";
import { arch, homedir, platform }                     from "node:os";
import { dirname, join, resolve as resolvePath }       from "node:path";
import { EventEmitter }                                from "node:events";

// ─── Binary Resolution ────────────────────────────────────────────────────────

const OS_MAP   = { win32: "win32", darwin: "darwin", linux: "linux" };
const ARCH_MAP = { x64: "x64", arm64: "arm64" };

const currentOS   = OS_MAP[platform()];
const currentArch = ARCH_MAP[arch()];

if (!currentOS || !currentArch) {
  throw new Error(
    `tray-hook: unsupported platform ${platform()}/${arch()}. ` +
    `Supported: win32/darwin/linux × x64/arm64.`
  );
}

const PLATFORM_PKG = `@phtdacosta/tray-hook-${currentOS}-${currentArch}`;
const BINARY_NAME  = currentOS === "win32" ? "tray-hook.exe" : "tray-hook";

const CMD_TIMEOUT      = 10_000;
const CRASH_WINDOW_MS  = 10_000;
const MAX_CRASHES      = 5;
const RESTART_DELAY_MS = 500;

function resolveBinaryPath() {
  try {
    const req = createRequire(import.meta.url);
    const pkg = req(`${PLATFORM_PKG}/package.json`);
    const bin = pkg?.bin?.[BINARY_NAME] ?? BINARY_NAME;
    return resolvePath(dirname(req.resolve(`${PLATFORM_PKG}/package.json`)), bin);
  } catch (cause) {
    throw new Error(
      `tray-hook: could not find native binary. ` +
      `Is '${PLATFORM_PKG}' installed?\nCause: ${cause.message}`
    );
  }
}

const BINARY_PATH = resolveBinaryPath();

// ─── Validation ───────────────────────────────────────────────────────────────

const MAX_ID_LEN    = 128;
const MAX_TITLE_LEN = 256;

function assertId(id) {
  if (typeof id !== "string" || id.length === 0 || id.length > MAX_ID_LEN)
    throw new TypeError(`id must be a non-empty string ≤ ${MAX_ID_LEN} chars, got: ${JSON.stringify(id)}`);
  if (/[\x00-\x1f]/.test(id))
    throw new TypeError("id must not contain control characters");
}

function assertTitle(title) {
  if (typeof title !== "string" || title.length === 0 || title.length > MAX_TITLE_LEN)
    throw new TypeError(`title must be a non-empty string ≤ ${MAX_TITLE_LEN} chars, got: ${JSON.stringify(title)}`);
}

function assertBoolean(name, value) {
  if (typeof value !== "boolean")
    throw new TypeError(`'${name}' must be a boolean, got: ${typeof value}`);
}

function assertString(name, value) {
  if (typeof value !== "string" || !value)
    throw new TypeError(`'${name}' must be a non-empty string`);
}

// ─── Autostart helpers ────────────────────────────────────────────────────────

function parseExecArgs(str) {
  const args = [];
  let cur = "", inQuote = false;
  for (const ch of str) {
    if (ch === '"' && !inQuote) { inQuote = true;  continue; }
    if (ch === '"' &&  inQuote) { inQuote = false; continue; }
    if (ch === " " && !inQuote) { if (cur) { args.push(cur); cur = ""; } }
    else cur += ch;
  }
  if (cur) args.push(cur);
  return args;
}

function buildPlist(appId, execPath) {
  const argTags = parseExecArgs(execPath)
    .map(a => `    <string>${a}</string>`)
    .join("\n");
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>${appId}</string>
  <key>ProgramArguments</key>
  <array>
${argTags}
  </array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
`;
}

function buildDesktopEntry(appId, execPath) {
  return `[Desktop Entry]
Type=Application
Name=${appId}
Exec=${execPath}
Hidden=false
NoDisplay=false
X-GNOME-Autostart-enabled=true
`;
}

// ─── Shadow state helpers ─────────────────────────────────────────────────────

/** Recursively resolve icon paths inside a setMenu template (deep-clones). */
function resolveTemplateIcons(items) {
  return items.map(item => {
    const clone = { ...item };
    if (clone.icon) clone.icon = resolvePath(clone.icon);
    if (Array.isArray(clone.items)) clone.items = resolveTemplateIcons(clone.items);
    return clone;
  });
}

/**
 * Walk a menu template tree and call updater(item) on the first item whose
 * id matches. Returns true if found.
 */
function updateTemplateItem(items, id, updater) {
  for (const item of items) {
    if (item.id === id) { updater(item); return true; }
    if (item.type === "submenu" && Array.isArray(item.items)) {
      if (updateTemplateItem(item.items, id, updater)) return true;
    }
  }
  return false;
}

// ─── Tray Class ───────────────────────────────────────────────────────────────

export class Tray extends EventEmitter {
  // Transport state
  #process      = null;
  #isReady      = false;
  #startPromise = null;
  #counter      = 0;
  #pending      = new Map(); // cmd_id → { resolve, reject, timer }
  #queue        = [];

  // Auto-restart state
  #autoRestart    = true;
  #intentionalStop = false;
  #crashCount     = 0;
  #firstCrashTime = null;

  // Shadow state — tracks the last known state for daemon-crash replay
  #replaying   = false;
  #shadowState = {
    icon:          null,   // last setIcon path (absolute)
    iconData:      null,   // last setIconData base64 (mutually exclusive with icon)
    tooltip:       null,
    trayTitle:     null,   // macOS only
    definedStates: {},     // last defineStates map (name → absolute path)
    currentState:  null,   // last setState name
    menu:          null,   // setMenu template, or null if built imperatively
    items:         new Map(), // id → full command payload
    itemOrder:     [],        // insertion order
  };

  constructor({ autoRestart = true } = {}) {
    super();
    this.#autoRestart = autoRestart;
  }

  // ── Lifecycle ──────────────────────────────────────────────────────────────

  /**
   * Spawn the Rust daemon and resolve when it signals "ready".
   * Idempotent — concurrent calls share the same Promise.
   */
  start() {
    if (this.#startPromise) return this.#startPromise;

    this.#startPromise = new Promise((resolve, reject) => {
      let readyAchieved = false;
      this.once("ready", () => { readyAchieved = true; });

      let proc;
      try {
        proc = spawn(BINARY_PATH, [], { stdio: ["pipe", "pipe", "inherit"] });
      } catch (err) {
        this.#startPromise = null;
        return reject(new Error(`tray-hook: failed to spawn daemon: ${err.message}`));
      }

      this.#process = proc;

      // stdout framing
      let buf = "";
      proc.stdout.on("data", chunk => {
        buf += chunk.toString();
        const lines = buf.split("\n");
        buf = lines.pop() ?? "";
        for (const line of lines) {
          if (!line.trim()) continue;
          let payload;
          try { payload = JSON.parse(line); }
          catch { this.emit("error", new Error(`tray-hook: malformed JSON from daemon: ${line}`)); continue; }
          this.#handleEvent(payload);
        }
      });

      // process exit
      proc.on("exit", code => {
        const wasReady      = readyAchieved;
        const wasIntentional = this.#intentionalStop;
        this.#intentionalStop = false;
        this.#isReady         = false;
        this.#process         = null;
        this.#startPromise    = null;
        this.#rejectAll(new Error(`tray-hook: daemon exited (code ${code})`));
        if (!wasReady) reject(new Error(`tray-hook: daemon exited before ready (code ${code})`));
        this.emit("exit", code);

        // Auto-restart logic — only on unexpected crashes
        if (this.#autoRestart && !wasIntentional && code !== 0 && wasReady) {
          const now = Date.now();
          if (this.#firstCrashTime === null || now - this.#firstCrashTime > CRASH_WINDOW_MS) {
            this.#firstCrashTime = now;
            this.#crashCount = 1;
          } else {
            this.#crashCount++;
          }

          if (this.#crashCount >= MAX_CRASHES) {
            this.#autoRestart = false;
            this.emit("error", new Error(
              `tray-hook: daemon crashed ${MAX_CRASHES} times within ${CRASH_WINDOW_MS}ms — auto-restart disabled`
            ));
            return;
          }

          setTimeout(() => {
            this.start()
              .then(() => this.#replayShadowState())
              .then(() => this.emit("restart"))
              .catch(err => this.emit("error", err));
          }, RESTART_DELAY_MS);
        }
      });

      // spawn / OS-level error
      proc.on("error", err => {
        this.#process      = null;
        this.#startPromise = null;
        reject(new Error(`tray-hook: daemon process error: ${err.message}`));
      });

      this.once("ready", resolve);
    });

    return this.#startPromise;
  }

  /**
   * Immediately kill the daemon and reject all in-flight commands.
   * Does not wait for exit — attach a "exit" listener if needed.
   */
  destroy() {
    this.#intentionalStop = true;
    this.#rejectAll(new Error("tray-hook: destroyed by caller"));
    this.#queue        = [];
    this.#isReady      = false;
    this.#startPromise = null;
    if (this.#process) { this.#process.kill(); this.#process = null; }
  }

  /** Permanently disable auto-restart without destroying the current daemon. */
  disableAutoRestart() {
    this.#autoRestart = false;
  }

  // ── Internal ───────────────────────────────────────────────────────────────

  #handleEvent(payload) {
    switch (payload.event) {
      case "ready":
        this.#isReady = true;
        this.emit("ready");
        for (const q of this.#queue) this.#write(q);
        this.#queue = [];
        break;

      case "click":
        this.emit("click", payload.id);
        break;

      case "check": {
        // Sync shadow state before surfacing to consumer
        const id = payload.id, checked = payload.checked;
        if (this.#shadowState.menu !== null) {
          updateTemplateItem(this.#shadowState.menu, id, item => { item.checked = checked; });
        } else {
          const cmd = this.#shadowState.items.get(id);
          if (cmd) cmd.checked = checked;
        }
        this.emit("check", id, checked);
        break;
      }

      case "tray_click":
        this.emit("tray_click", payload.button);
        break;

      case "ack": {
        const e = this.#pending.get(payload.cmd_id);
        if (e) { clearTimeout(e.timer); e.resolve(); this.#pending.delete(payload.cmd_id); }
        break;
      }

      case "autostart": {
        const e = this.#pending.get(payload.cmd_id);
        if (e) { clearTimeout(e.timer); e.resolve(payload.enabled); this.#pending.delete(payload.cmd_id); }
        break;
      }

      case "error": {
        const e = payload.cmd_id && this.#pending.get(payload.cmd_id);
        if (e) {
          clearTimeout(e.timer);
          e.reject(new Error(payload.message));
          this.#pending.delete(payload.cmd_id);
        } else {
          this.emit("error", new Error(`[daemon] ${payload.message}`));
        }
        break;
      }

      default:
        this.emit("error", new Error(`tray-hook: unknown event type '${payload.event}'`));
    }
  }

  #write(cmd) {
    if (!this.#process?.stdin.writable) {
      const e = this.#pending.get(cmd.cmd_id);
      if (e) { clearTimeout(e.timer); e.reject(new Error("tray-hook: daemon stdin is not writable")); this.#pending.delete(cmd.cmd_id); }
      return;
    }
    this.#process.stdin.write(JSON.stringify(cmd) + "\n");
  }

  #rejectAll(err) {
    for (const [id, e] of this.#pending) {
      clearTimeout(e.timer); e.reject(err); this.#pending.delete(id);
    }
  }

  /** Replay all tracked state onto a freshly started daemon. Must be await-chained. */
  async #replayShadowState() {
    this.#replaying = true;
    try {
      const s = this.#shadowState;
      if (s.icon)          await this.setIcon(s.icon);
      else if (s.iconData) await this.setIconData(s.iconData);
      if (s.tooltip)       await this.setTooltip(s.tooltip);
      if (s.trayTitle && currentOS === "darwin") await this.setTrayTitle(s.trayTitle);
      if (Object.keys(s.definedStates).length > 0) await this.defineStates(s.definedStates);
      if (s.currentState) await this.setState(s.currentState);
      if (s.menu !== null) {
        await this.setMenu(s.menu);
      } else {
        for (const id of s.itemOrder) {
          const cmd = s.items.get(id);
          if (cmd) await this.send({ ...cmd });
        }
      }
    } finally {
      this.#replaying = false;
    }
  }

  /**
   * Low-level transport. Sends a raw command and returns a Promise that
   * resolves on ack, resolves with a value on typed responses (e.g. autostart),
   * or rejects on error/timeout.
   */
  send(cmd) {
    if (!this.#process && !this.#startPromise)
      return Promise.reject(new Error("tray-hook: call start() before sending commands"));

    const cmd_id  = String(++this.#counter);
    const payload = { ...cmd, cmd_id };

    const promise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(cmd_id);
        reject(new Error(`tray-hook: command '${cmd.action}' (cmd_id=${cmd_id}) timed out after ${CMD_TIMEOUT}ms`));
      }, CMD_TIMEOUT);
      this.#pending.set(cmd_id, { resolve, reject, timer });
    });

    if (!this.#isReady) this.#queue.push(payload);
    else this.#write(payload);

    return promise;
  }

  // ── Tray-level controls ────────────────────────────────────────────────────

  setIcon(iconPath) {
    assertString("iconPath", iconPath);
    const abs = resolvePath(iconPath);
    return this.send({ action: "set_icon", path: abs }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.icon     = abs;
      this.#shadowState.iconData = null;
    });
  }

  setIconData(base64) {
    assertString("base64", base64);
    const data = base64.replace(/^data:image\/\w+;base64,/, "");
    return this.send({ action: "set_icon_data", data }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.iconData = data;
      this.#shadowState.icon     = null;
    });
  }

  setTooltip(title) {
    assertTitle(title);
    return this.send({ action: "set_tooltip", title }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.tooltip = title;
    });
  }

  /** macOS only. Rejects on other platforms via daemon error. */
  setTrayTitle(title) {
    assertTitle(title);
    return this.send({ action: "set_tray_title", title }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.trayTitle = title;
    });
  }

  // ── Menu creation ──────────────────────────────────────────────────────────

  add(id, title, { enabled = true, parent_id, icon } = {}) {
    assertId(id);
    assertTitle(title);
    assertBoolean("enabled", enabled);
    if (parent_id !== undefined) assertId(parent_id);
    if (this.#shadowState.menu !== null)
      return Promise.reject(new Error(
        "tray-hook: cannot mutate menu structure imperatively after setMenu() — update your template and call setMenu() again"
      ));
    const resolvedIcon = icon ? resolvePath(icon) : undefined;
    const cmd = { action: "add", id, title, enabled, parent_id, icon: resolvedIcon };
    return this.send(cmd).then(() => {
      if (this.#replaying) return;
      if (!this.#shadowState.items.has(id)) this.#shadowState.itemOrder.push(id);
      this.#shadowState.items.set(id, { ...cmd });
    });
  }

  addCheck(id, title, { checked = false, enabled = true, parent_id, icon } = {}) {
    assertId(id);
    assertTitle(title);
    assertBoolean("checked", checked);
    assertBoolean("enabled", enabled);
    if (parent_id !== undefined) assertId(parent_id);
    if (this.#shadowState.menu !== null)
      return Promise.reject(new Error(
        "tray-hook: cannot mutate menu structure imperatively after setMenu() — update your template and call setMenu() again"
      ));
    const resolvedIcon = icon ? resolvePath(icon) : undefined;
    const cmd = { action: "add_check", id, title, checked, enabled, parent_id, icon: resolvedIcon };
    return this.send(cmd).then(() => {
      if (this.#replaying) return;
      if (!this.#shadowState.items.has(id)) this.#shadowState.itemOrder.push(id);
      this.#shadowState.items.set(id, { ...cmd });
    });
  }

  addSubmenu(id, title, { enabled = true, parent_id } = {}) {
    assertId(id);
    assertTitle(title);
    assertBoolean("enabled", enabled);
    if (parent_id !== undefined) assertId(parent_id);
    if (this.#shadowState.menu !== null)
      return Promise.reject(new Error(
        "tray-hook: cannot mutate menu structure imperatively after setMenu() — update your template and call setMenu() again"
      ));
    const cmd = { action: "add_submenu", id, title, enabled, parent_id };
    return this.send(cmd).then(() => {
      if (this.#replaying) return;
      if (!this.#shadowState.items.has(id)) this.#shadowState.itemOrder.push(id);
      this.#shadowState.items.set(id, { ...cmd });
    });
  }

  addSeparator(id, { parent_id } = {}) {
    assertId(id);
    if (parent_id !== undefined) assertId(parent_id);
    if (this.#shadowState.menu !== null)
      return Promise.reject(new Error(
        "tray-hook: cannot mutate menu structure imperatively after setMenu() — update your template and call setMenu() again"
      ));
    const cmd = { action: "add_separator", id, parent_id };
    return this.send(cmd).then(() => {
      if (this.#replaying) return;
      if (!this.#shadowState.items.has(id)) this.#shadowState.itemOrder.push(id);
      this.#shadowState.items.set(id, { ...cmd });
    });
  }

  // ── Menu mutation ──────────────────────────────────────────────────────────

  rename(id, title) {
    assertId(id);
    assertTitle(title);
    return this.send({ action: "rename", id, title }).then(() => {
      if (this.#replaying) return;
      if (this.#shadowState.menu !== null) {
        updateTemplateItem(this.#shadowState.menu, id, item => { item.title = title; });
      } else {
        const cmd = this.#shadowState.items.get(id);
        if (cmd) cmd.title = title;
      }
    });
  }

  setEnabled(id, enabled) {
    assertId(id);
    assertBoolean("enabled", enabled);
    return this.send({ action: "set_enabled", id, enabled }).then(() => {
      if (this.#replaying) return;
      if (this.#shadowState.menu !== null) {
        updateTemplateItem(this.#shadowState.menu, id, item => { item.enabled = enabled; });
      } else {
        const cmd = this.#shadowState.items.get(id);
        if (cmd) cmd.enabled = enabled;
      }
    });
  }

  setChecked(id, checked) {
    assertId(id);
    assertBoolean("checked", checked);
    return this.send({ action: "set_checked", id, checked }).then(() => {
      if (this.#replaying) return;
      if (this.#shadowState.menu !== null) {
        updateTemplateItem(this.#shadowState.menu, id, item => { item.checked = checked; });
      } else {
        const cmd = this.#shadowState.items.get(id);
        if (cmd) cmd.checked = checked;
      }
    });
  }

  toggle(id) {
    assertId(id);
    return this.send({ action: "toggle", id }).then(() => {
      if (this.#replaying) return;
      if (this.#shadowState.menu !== null) {
        updateTemplateItem(this.#shadowState.menu, id, item => { item.checked = !item.checked; });
      } else {
        const cmd = this.#shadowState.items.get(id);
        if (cmd) cmd.checked = !cmd.checked;
      }
    });
  }

  remove(id) {
    assertId(id);
    if (this.#shadowState.menu !== null)
      return Promise.reject(new Error(
        "tray-hook: cannot mutate menu structure imperatively after setMenu() — update your template and call setMenu() again"
      ));
    return this.send({ action: "remove", id }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.items.delete(id);
      this.#shadowState.itemOrder = this.#shadowState.itemOrder.filter(i => i !== id);
    });
  }

  clear() {
    return this.send({ action: "clear" }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.items.clear();
      this.#shadowState.itemOrder = [];
      this.#shadowState.menu      = null;
    });
  }

  // ── Declarative menu ──────────────────────────────────────────────────────

  setMenu(template) {
    if (!Array.isArray(template)) throw new TypeError("template must be an array");
    const resolved = resolveTemplateIcons(template);
    return this.send({ action: "set_menu", items: resolved }).then(() => {
      if (this.#replaying) return;
      // Deep-clone via JSON to prevent external mutations corrupting shadow state
      this.#shadowState.menu = JSON.parse(JSON.stringify(resolved));
      this.#shadowState.items.clear();
      this.#shadowState.itemOrder = [];
    });
  }

  // ── Icon states ───────────────────────────────────────────────────────────

  defineStates(states) {
    if (typeof states !== "object" || states === null) throw new TypeError("states must be an object");
    const resolved = Object.fromEntries(
      Object.entries(states).map(([k, v]) => [k, resolvePath(v)])
    );
    return this.send({ action: "define_states", states: resolved }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.definedStates = { ...resolved };
    });
  }

  setState(stateName) {
    assertString("stateName", stateName);
    return this.send({ action: "set_state", state_name: stateName }).then(() => {
      if (this.#replaying) return;
      this.#shadowState.currentState = stateName;
    });
  }

  // ── Autostart ─────────────────────────────────────────────────────────────

  async setAutostart(appId, execPath, enabled) {
    assertString("appId", appId);
    assertString("execPath", execPath);
    assertBoolean("enabled", enabled);

    if (currentOS === "darwin") {
      const dir  = join(homedir(), "Library", "LaunchAgents");
      const file = join(dir, `${appId}.plist`);
      if (enabled) { mkdirSync(dir, { recursive: true }); writeFileSync(file, buildPlist(appId, execPath)); }
      else          { try { unlinkSync(file); } catch {} }
      return;
    }

    if (currentOS === "linux") {
      const dir  = join(homedir(), ".config", "autostart");
      const file = join(dir, `${appId}.desktop`);
      if (enabled) { mkdirSync(dir, { recursive: true }); writeFileSync(file, buildDesktopEntry(appId, execPath)); }
      else          { try { unlinkSync(file); } catch {} }
      return;
    }

    // Windows — delegate to daemon (requires registry access)
    return this.send({ action: "set_autostart", app_id: appId, exec_path: execPath, enabled });
  }

  async getAutostart(appId) {
    assertString("appId", appId);

    if (currentOS === "darwin")
      return existsSync(join(homedir(), "Library", "LaunchAgents", `${appId}.plist`));

    if (currentOS === "linux")
      return existsSync(join(homedir(), ".config", "autostart", `${appId}.desktop`));

    // Windows — daemon resolves with boolean via "autostart" event
    return this.send({ action: "get_autostart", app_id: appId });
  }

  // ── Lifecycle ─────────────────────────────────────────────────────────────

  quit() {
    // The daemon exits immediately on receipt — it never sends an ack.
    // Write directly without a pending entry to avoid a spurious rejection.
    this.#write({ cmd_id: "__quit__", action: "quit" });
    return Promise.resolve();
  }
}

export const createTray = (opts) => new Tray(opts);
