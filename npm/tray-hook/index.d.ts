import { EventEmitter } from "node:events";

export interface AddOptions {
  enabled?: boolean;
  parent_id?: string;
}

export interface AddCheckOptions {
  checked?: boolean;
  enabled?: boolean;
  parent_id?: string;
}

export interface AddSubmenuOptions {
  enabled?: boolean;
  parent_id?: string;
}

export interface AddSeparatorOptions {
  parent_id?: string;
}

export interface TrayEvents {
  /** Daemon is up and accepting commands. */
  ready: [];
  /** A regular menu item was clicked. */
  click: [id: string];
  /** A check-menu item was toggled. `checked` is the new state. */
  check: [id: string, checked: boolean];
  /** Daemon process exited. `code` is null if killed by signal. */
  exit: [code: number | null];
  /** Unmatched or protocol-level error from the daemon. */
  error: [err: Error];
}

export declare class Tray extends EventEmitter {
  // ── Lifecycle ──────────────────────────────────────────────────────────────
  /** Spawn the Rust daemon and resolve when it signals ready. Idempotent. */
  start(): Promise<void>;
  /** Immediately kill the daemon and reject all in-flight commands. */
  destroy(): void;

  // ── Generic escape hatch ──────────────────────────────────────────────────
  send(cmd: Record<string, unknown>): Promise<void>;

  // ── Menu creation ──────────────────────────────────────────────────────────
  add(id: string, title: string, options?: AddOptions): Promise<void>;
  addCheck(id: string, title: string, options?: AddCheckOptions): Promise<void>;
  addSubmenu(id: string, title: string, options?: AddSubmenuOptions): Promise<void>;
  addSeparator(id: string, options?: AddSeparatorOptions): Promise<void>;

  // ── Menu mutation ──────────────────────────────────────────────────────────
  rename(id: string, title: string): Promise<void>;
  setEnabled(id: string, enabled: boolean): Promise<void>;
  setChecked(id: string, checked: boolean): Promise<void>;
  toggle(id: string): Promise<void>;
  remove(id: string): Promise<void>;
  clear(): Promise<void>;

  // ── Tray-level controls ────────────────────────────────────────────────────
  setIcon(iconPath: string): Promise<void>;
  setTooltip(title: string): Promise<void>;
  /** macOS only. Rejects on other platforms via daemon error. */
  setTrayTitle(title: string): Promise<void>;

  // ── Lifecycle ──────────────────────────────────────────────────────────────
  quit(): Promise<void>;

  // ── Typed event emitter overloads ─────────────────────────────────────────
  on<K extends keyof TrayEvents>(event: K, listener: (...args: TrayEvents[K]) => void): this;
  once<K extends keyof TrayEvents>(event: K, listener: (...args: TrayEvents[K]) => void): this;
  off<K extends keyof TrayEvents>(event: K, listener: (...args: TrayEvents[K]) => void): this;
  emit<K extends keyof TrayEvents>(event: K, ...args: TrayEvents[K]): boolean;
}

/** Convenience factory. Equivalent to `new Tray()`. */
export declare function createTray(): Tray;