import fs from "node:fs";
import path from "node:path";
import process from "node:process";

const projectRoot = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const omarchyRoot = process.env.OMARCHY_PATH || "/usr/share/omarchy";
const shellRoot = path.join(omarchyRoot, "shell");
const parityPath = path.join(projectRoot, "parity.json");

function walk(dir) {
  if (!fs.existsSync(dir)) return [];
  const result = [];
  for (const name of fs.readdirSync(dir).sort()) {
    const file = path.join(dir, name);
    const stat = fs.statSync(file);
    if (stat.isDirectory()) result.push(...walk(file));
    else if (name === "manifest.json" || name.endsWith(".manifest.json")) result.push(file);
  }
  return result;
}

function fail(message) {
  console.error(`OMARCHY_GPUI_PARITY_AUDIT_PENDING: ${message}`);
  process.exitCode = 1;
}

if (!fs.existsSync(shellRoot)) fail(`reference shell not found: ${shellRoot}`);
if (!fs.existsSync(parityPath)) fail(`parity registry not found: ${parityPath}`);

let registry = { plugins: {}, verifiedFeatures: [] };
try {
  registry = JSON.parse(fs.readFileSync(parityPath, "utf8"));
} catch (error) {
  fail(`invalid parity registry: ${error.message}`);
}

const manifests = [];
for (const file of walk(shellRoot)) {
  try {
    const manifest = JSON.parse(fs.readFileSync(file, "utf8"));
    if (manifest && typeof manifest.id === "string") manifests.push(manifest);
  } catch (error) {
    fail(`invalid reference manifest ${file}: ${error.message}`);
  }
}

const ids = [...new Set(manifests.map((manifest) => manifest.id))].sort();
const missing = ids.filter((id) => registry.plugins?.[id]?.status !== "verified");
const requiredShellMethods = [
  "ping",
  "summon",
  "hide",
  "toggle",
  "call",
  "rescanPlugins",
  "reloadConfig",
  "toggleBarTransparency",
  "setPluginEnabled",
  "enablePlugin",
  "listPlugins",
  "togglePanelAt",
];
const ipcSource = fs.readFileSync(path.join(projectRoot, "src/ipc.rs"), "utf8");
const missingMethods = requiredShellMethods.filter((method) => !ipcSource.includes(`"${method}"`));
const requiredFeatures = ["config", "plugin-registry", "ipc", "bar", "panels", "services", "overlays", "session"];
const missingFeatures = requiredFeatures.filter((feature) => !registry.verifiedFeatures?.includes(feature));

console.log(`reference_plugins=${ids.length}`);
console.log(`verified_plugins=${ids.length - missing.length}`);
console.log(`verified_features=${registry.verifiedFeatures?.length || 0}`);
if (missing.length) console.error(`unverified_plugins=${missing.join(",")}`);
if (missingMethods.length) console.error(`missing_shell_methods=${missingMethods.join(",")}`);
if (missingFeatures.length) console.error(`unverified_features=${missingFeatures.join(",")}`);

if (missing.length || missingMethods.length || missingFeatures.length) {
  fail("complete plugin and behavior coverage is not yet recorded");
} else {
  console.log("OMARCHY_GPUI_PARITY_AUDIT_OK");
}
