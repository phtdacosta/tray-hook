/**
 * basic.js — minimal tray-hook example
 *
 * Shows a tray icon with a simple menu that demonstrates:
 *   - Regular items and click handling
 *   - Check items and toggle handling
 *   - Dynamic label updates
 *   - Graceful shutdown
 *
 * Run:
 *   bun examples/basic.js
 *   node examples/basic.js
 */

import { createTray } from "tray-hook";

const tray = createTray();

tray.on("error", (err) => console.error("[tray-hook]", err.message));

await tray.start();

// await tray.setIcon("./icon.png"); // replace with your own icon path
await tray.setTooltip("My App");

await tray.add("status", "Server: Stopped", { enabled: false });
await tray.add("toggle", "Start Server");
await tray.addSeparator("sep-1");
await tray.addCheck("autostart", "Auto-Start on Boot");
await tray.addSeparator("sep-2");
await tray.add("quit", "Quit");

let running = false;

tray.on("click", async (id) => {
  if (id === "toggle") {
    running = !running;
    if (running) {
      await tray.rename("status", "Server: Running ✓");
      await tray.rename("toggle", "Stop Server");
      await tray.setTooltip("My App — Running");
    } else {
      await tray.rename("status", "Server: Stopped");
      await tray.rename("toggle", "Start Server");
      await tray.setTooltip("My App");
    }
  }

  if (id === "quit") {
    await tray.quit();
    process.exit(0);
  }
});

tray.on("check", (id, checked) => {
  if (id === "autostart") {
    console.log(`Auto-start: ${checked ? "enabled" : "disabled"}`);
  }
});