/**
 * tray-hook — JavaScript/TypeScript host for the tray-hook Rust daemon.
 *
 * Events emitted on Tray instance:
 *   "ready"              — daemon is up and accepting commands
 *   "click"  (id)        — a regular menu item was activated
 *   "check"  (id, bool)  — a check item was toggled; bool is new state
 *   "exit"   (code)      — daemon process exited (expected or unexpected)
 *   "error"  (Error)     — unmatched / protocol-level error from daemon
 */

import { spawn } from "node:child_process";
import { platform, arch } from "node:os";
import { dirname, resolve as resolvePath } from "node:path";
import { createRequire } from "node:module";
import { EventEmitter } from "node:events";

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
const CMD_TIMEOUT  = 10_000; // ms to wait for an ack before rejecting

function resolveBinaryPath() {
  try {
    const req = createRequire(import.meta.url);
    // Each platform package exposes a `bin` field in its own package.json.
    // We honour that contract rather than hard-coding a relative path.
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
  if (typeof id !== "string" || id.length === 0 || id.length > MAX_ID_LEN) {
    throw new TypeError(
      `id must be a non-empty string ≤ ${MAX_ID_LEN} chars, got: ${JSON.stringify(id)}`
    );
  }
  if (/[\x00-\x1f]/.test(id)) {
    throw new TypeError("id must not contain control characters");
  }
}

function assertTitle(title) {
  if (typeof title !== "string" || title.length === 0 || title.length > MAX_TITLE_LEN) {
    throw new TypeError(
      `title must be a non-empty string ≤ ${MAX_TITLE_LEN} chars, got: ${JSON.stringify(title)}`
    );
  }
}

function assertBoolean(name, value) {
  if (typeof value !== "boolean") {
    throw new TypeError(`'${name}' must be a boolean, got: ${typeof value}`);
  }
}

// ─── Tray Class ───────────────────────────────────────────────────────────────

export class Tray extends EventEmitter {
  #process      = null;
  #isReady      = false;
  #startPromise = null;  // guards against concurrent start() calls
  #counter      = 0;
  #pending      = new Map(); // cmd_id → { resolve, reject, timer }
  #queue        = [];        // commands buffered before "ready"

  // ── Lifecycle ──────────────────────────────────────────────────────────────

  /**
   * Spawn the Rust daemon and resolve when it signals "ready".
   * Safe to call multiple times — subsequent calls return the same Promise.
   */
  start() {
    if (this.#startPromise) return this.#startPromise;

    this.#startPromise = new Promise((resolve, reject) => {
      // Capture whether "ready" was ever emitted so the exit handler can
      // distinguish a pre-ready crash from a normal post-run exit.
      let readyAchieved = false;
      this.once("ready", () => { readyAchieved = true; });

      let daemonProcess;
      try {
        daemonProcess = spawn(BINARY_PATH, [], {
          stdio: ["pipe", "pipe", "inherit"],
        });
      } catch (err) {
        this.#startPromise = null;
        return reject(new Error(`tray-hook: failed to spawn daemon: ${err.message}`));
      }

      this.#process = daemonProcess;

      // ── stdout framing ──────────────────────────────────────────────────
      let buf = "";
      daemonProcess.stdout.on("data", (chunk) => {
        buf += chunk.toString();
        const lines = buf.split("\n");
        buf = lines.pop() ?? ""; // keep incomplete tail
        for (const line of lines) {
          if (!line.trim()) continue;
          let payload;
          try {
            payload = JSON.parse(line);
          } catch {
            this.emit("error", new Error(`tray-hook: malformed JSON from daemon: ${line}`));
            continue;
          }
          this.#handleEvent(payload);
        }
      });

      // ── process exit ────────────────────────────────────────────────────
      daemonProcess.on("exit", (code) => {
        const wasReady = readyAchieved;
        this.#isReady      = false;
        this.#process      = null;
        this.#startPromise = null;
        this.#rejectAll(new Error(`tray-hook: daemon exited (code ${code})`));
        // Only reject the start() promise if the daemon died before ready.
        if (!wasReady) reject(new Error(`tray-hook: daemon exited before ready (code ${code})`));
        this.emit("exit", code);
      });

      // ── spawn / OS-level error ──────────────────────────────────────────
      daemonProcess.on("error", (err) => {
        this.#process      = null;
        this.#startPromise = null;
        reject(new Error(`tray-hook: daemon process error: ${err.message}`));
      });

      // ── ready gate ──────────────────────────────────────────────────────
      this.once("ready", resolve);
    });

    return this.#startPromise;
  }

  /**
   * Immediately kill the daemon and reject all in-flight commands.
   * Does not wait for the process to exit; attach a listener on "exit" if needed.
   */
  destroy() {
    this.#rejectAll(new Error("tray-hook: destroyed by caller"));
    this.#queue        = [];
    this.#isReady      = false;
    this.#startPromise = null;
    if (this.#process) {
      this.#process.kill();
      this.#process = null;
    }
  }

  // ── Internal ───────────────────────────────────────────────────────────────

  #handleEvent(payload) {
    switch (payload.event) {
      case "ready":
        this.#isReady = true;
        this.emit("ready");
        // Flush commands that arrived before the daemon was ready.
        for (const queued of this.#queue) this.#write(queued);
        this.#queue = [];
        break;

      case "click":
        this.emit("click", payload.id);
        break;

      case "check":
        this.emit("check", payload.id, payload.checked);
        break;

      case "ack": {
        const entry = this.#pending.get(payload.cmd_id);
        if (entry) {
          clearTimeout(entry.timer);
          entry.resolve();
          this.#pending.delete(payload.cmd_id);
        }
        break;
      }

      case "error": {
        const entry = payload.cmd_id && this.#pending.get(payload.cmd_id);
        if (entry) {
          clearTimeout(entry.timer);
          entry.reject(new Error(payload.message));
          this.#pending.delete(payload.cmd_id);
        } else {
          // Unmatched daemon error — surface as an EventEmitter "error" event.
          // Without a listener attached, Node.js will throw (standard behaviour).
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
      // Stdin is gone — reject immediately rather than waiting for the timeout.
      const entry = this.#pending.get(cmd.cmd_id);
      if (entry) {
        clearTimeout(entry.timer);
        entry.reject(new Error("tray-hook: daemon stdin is not writable"));
        this.#pending.delete(cmd.cmd_id);
      }
      return;
    }
    this.#process.stdin.write(JSON.stringify(cmd) + "\n");
  }

  #rejectAll(err) {
    for (const [id, entry] of this.#pending) {
      clearTimeout(entry.timer);
      entry.reject(err);
      this.#pending.delete(id);
    }
  }

  /**
   * Send a command to the daemon and return a Promise that resolves on ack
   * or rejects on error or timeout.
   */
  send(cmd) {
    if (!this.#process && !this.#startPromise) {
      return Promise.reject(
        new Error("tray-hook: call start() before sending commands")
      );
    }

    const cmd_id  = String(++this.#counter);
    const payload = { ...cmd, cmd_id };

    const promise = new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#pending.delete(cmd_id);
        reject(
          new Error(
            `tray-hook: command '${cmd.action}' (cmd_id=${cmd_id}) timed out after ${CMD_TIMEOUT}ms`
          )
        );
      }, CMD_TIMEOUT);

      this.#pending.set(cmd_id, { resolve, reject, timer });
    });

    if (!this.#isReady) {
      this.#queue.push(payload);
    } else {
      this.#write(payload);
    }

    return promise;
  }

  // ── Public API ─────────────────────────────────────────────────────────────

  add(id, title, { enabled = true, parent_id } = {}) {
    assertId(id);
    assertTitle(title);
    assertBoolean("enabled", enabled);
    if (parent_id !== undefined) assertId(parent_id);
    return this.send({ action: "add", id, title, enabled, parent_id });
  }

  addCheck(id, title, { checked = false, enabled = true, parent_id } = {}) {
    assertId(id);
    assertTitle(title);
    assertBoolean("checked", checked);
    assertBoolean("enabled", enabled);
    if (parent_id !== undefined) assertId(parent_id);
    return this.send({ action: "add_check", id, title, checked, enabled, parent_id });
  }

  addSubmenu(id, title, { enabled = true, parent_id } = {}) {
    assertId(id);
    assertTitle(title);
    assertBoolean("enabled", enabled);
    if (parent_id !== undefined) assertId(parent_id);
    return this.send({ action: "add_submenu", id, title, enabled, parent_id });
  }

  addSeparator(id, { parent_id } = {}) {
    assertId(id);
    if (parent_id !== undefined) assertId(parent_id);
    return this.send({ action: "add_separator", id, parent_id });
  }

  rename(id, title) {
    assertId(id);
    assertTitle(title);
    return this.send({ action: "rename", id, title });
  }

  setEnabled(id, enabled) {
    assertId(id);
    assertBoolean("enabled", enabled);
    return this.send({ action: "set_enabled", id, enabled });
  }

  setChecked(id, checked) {
    assertId(id);
    assertBoolean("checked", checked);
    return this.send({ action: "set_checked", id, checked });
  }

  toggle(id) {
    assertId(id);
    return this.send({ action: "toggle", id });
  }

  remove(id) {
    assertId(id);
    return this.send({ action: "remove", id });
  }

  clear() {
    return this.send({ action: "clear" });
  }

  setIcon(iconPath) {
    if (typeof iconPath !== "string" || !iconPath) {
      throw new TypeError("iconPath must be a non-empty string");
    }
    return this.send({ action: "set_icon", path: resolvePath(iconPath) });
  }

  setTooltip(title) {
    assertTitle(title);
    return this.send({ action: "set_tooltip", title });
  }

  /** macOS only. Rejects on other platforms via daemon error. */
  setTrayTitle(title) {
    assertTitle(title);
    return this.send({ action: "set_tray_title", title });
  }

  /** Gracefully ask the daemon to exit. */
  quit() {
    // The daemon exits immediately after receiving this — it never sends an ack.
    // Write directly without registering a pending request to avoid a spurious rejection.
    this.#write({ cmd_id: "__quit__", action: "quit" });
    return Promise.resolve();
  }
}

export function createTray() {
  return new Tray();
}