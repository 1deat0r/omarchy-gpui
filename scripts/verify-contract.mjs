import { readFileSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const requiredFiles = [
  "Cargo.toml",
  "assets/default-shell.json",
  "src/config.rs",
  "src/ipc.rs",
  "src/main.rs",
  "src/ui.rs",
];

for (const relative of requiredFiles) {
  const path = resolve(root, relative);
  statSync(path);
}

const cargo = readFileSync(resolve(root, "Cargo.toml"), "utf8");
if (!cargo.includes('package = "gpui"') || !cargo.includes('package = "gpui_platform"')) {
  throw new Error("Cargo.toml does not pin both GPUI packages");
}

const config = JSON.parse(readFileSync(resolve(root, "assets/default-shell.json"), "utf8"));
if (config.version !== 1) throw new Error("fallback shell config is not version 1");
for (const section of ["left", "center", "right"]) {
  if (!Array.isArray(config.bar?.layout?.[section])) {
    throw new Error(`missing bar layout section: ${section}`);
  }
}

const main = readFileSync(resolve(root, "src/main.rs"), "utf8");
for (const marker of ["LayerShellOptions", "OMARCHY_GPUI_CONTRACT_RUNTIME_OK", "--smoke"]) {
  if (!main.includes(marker)) throw new Error(`missing implementation marker: ${marker}`);
}

console.log("OMARCHY_GPUI_CONTRACT_OK");
