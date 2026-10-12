import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { existsSync } from "node:fs";
const directory = path.dirname(fileURLToPath(import.meta.url));
const compiler = ["vscode", "desktop"].map(client => path.resolve(directory, `../../${client}/node_modules/typescript/bin/tsc`)).find(existsSync);
if (!compiler) throw new Error("Install dependencies in packages/vscode or packages/desktop before building the shared channel");
execFileSync(process.execPath, [compiler,
  path.join(directory, "src/channel.ts"), "--outDir", directory, "--target", "ES2022", "--module", "ES2022", "--lib", "ES2022", "--strict", "--declaration", "--skipLibCheck"], { stdio: "inherit" });
