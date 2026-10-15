// Convert the shared PNG master into a complete macOS app icon.
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "darwin") {
  throw new Error("Regenerate the .icns file on macOS using sips and iconutil");
}
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = path.join(root, "icons", "icon.png");
const destination = path.join(root, "icons", "icon.icns");
const temporary = mkdtempSync(path.join(os.tmpdir(), "crab-computer-use-icon-"));
const iconset = path.join(temporary, "icon.iconset");
try {
  mkdirSync(iconset);
  for (const size of [16, 32, 128, 256, 512]) {
    for (const scale of [1, 2]) {
      const pixels = String(size * scale);
      const name = `icon_${size}x${size}${scale === 2 ? "@2x" : ""}.png`;
      execFileSync("/usr/bin/sips", ["-z", pixels, pixels, source, "--out", path.join(iconset, name)], { stdio: "pipe" });
    }
  }
  execFileSync("/usr/bin/iconutil", ["-c", "icns", iconset, "-o", destination], { stdio: "inherit" });
  console.log(`Computer Use icon: ${destination}`);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
