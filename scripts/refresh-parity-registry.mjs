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
    if (fs.statSync(file).isDirectory()) result.push(...walk(file));
    else if (name === "manifest.json" || name.endsWith(".manifest.json")) result.push(file);
  }
  return result;
}

const existing = JSON.parse(fs.readFileSync(parityPath, "utf8"));
const plugins = {};
for (const file of walk(shellRoot)) {
  const manifest = JSON.parse(fs.readFileSync(file, "utf8"));
  if (!manifest || typeof manifest.id !== "string") continue;
  const prior = existing.plugins?.[manifest.id] || {};
  plugins[manifest.id] = {
    referenceEntryPoints: manifest.entryPoints || {},
    referenceKinds: manifest.kinds || [],
    referenceManifest: path.relative(shellRoot, file),
    status: prior.status || "pending",
    gpuiModule: prior.gpuiModule || null,
    evidence: prior.evidence || [],
  };
}

const next = {
  schemaVersion: 1,
  reference: shellRoot,
  referenceOmarchyVersion: existing.referenceOmarchyVersion || null,
  plugins: Object.fromEntries(Object.entries(plugins).sort(([left], [right]) => left.localeCompare(right))),
  verifiedFeatures: existing.verifiedFeatures || [],
};
fs.writeFileSync(parityPath, `${JSON.stringify(next, null, 2)}\n`);
console.log(`OMARCHY_GPUI_PARITY_REGISTRY_REFRESHED: plugins=${Object.keys(plugins).length}`);
