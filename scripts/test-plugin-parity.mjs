import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const referenceRoot = process.env.OMARCHY_PATH || "/usr/share/omarchy";
const shellRoot = path.join(referenceRoot, "shell");
const parity = JSON.parse(readFileSync(path.join(projectRoot, "parity.json"), "utf8"));
const source = [
  readFileSync(path.join(projectRoot, "src/main.rs"), "utf8"),
  readFileSync(path.join(projectRoot, "src/plugins.rs"), "utf8"),
  readFileSync(path.join(projectRoot, "src/ipc.rs"), "utf8"),
  readFileSync(path.join(projectRoot, "src/ui.rs"), "utf8"),
].join("\n");
const failures = [];

function walk(dir) {
  if (!existsSync(dir)) return [];
  return readdirSync(dir).sort().flatMap((name) => {
    const file = path.join(dir, name);
    return statSync(file).isDirectory()
      ? walk(file)
      : (name === "manifest.json" || name.endsWith(".manifest.json") ? [file] : []);
  });
}

const entries = walk(shellRoot).map((file) => ({
  file,
  manifest: JSON.parse(readFileSync(file, "utf8")),
})).filter((entry) => entry.manifest && typeof entry.manifest.id === "string");

for (const { file, manifest } of entries) {
  const record = parity.plugins?.[manifest.id];
  if (!record) {
    failures.push(manifest.id + ": missing from parity registry");
    continue;
  }
  if (record.referenceManifest !== path.relative(shellRoot, file)) {
    failures.push(manifest.id + ": reference manifest path drift");
  }
  if (record.status !== "verified") {
    failures.push(manifest.id + ": registry status is " + record.status);
  }
  if (!source.includes('"' + manifest.id + '"')) {
    failures.push(manifest.id + ": no GPUI source route");
  }

  for (const kind of manifest.kinds || []) {
    if ((kind === "overlay" || kind === "panel")
      && (!source.includes("fn is_fullscreen_overlay") || !source.includes('"' + manifest.id + '"'))) {
      failures.push(manifest.id + ": no fullscreen GPUI surface marker");
    }
    if (kind === "bar-widget" && !source.includes('"' + manifest.id + '"')) {
      failures.push(manifest.id + ": no bar-widget renderer marker");
    }
    if (kind === "service" && !source.includes('"' + manifest.id + '"')) {
      failures.push(manifest.id + ": no service adapter marker");
    }
  }
}

const cargo = spawnSync(
  "/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo",
  ["test", "--offline"],
  {
    cwd: projectRoot,
    env: {
      ...process.env,
      PATH: [
        "/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin",
        process.env.PATH || "",
      ].join(":"),
    },
    encoding: "utf8",
    timeout: 120000,
  },
);
if (cargo.status !== 0) failures.push("Rust feature tests failed");

console.log("reference_plugins=" + entries.length);
console.log("registry_plugins=" + Object.keys(parity.plugins || {}).length);
console.log("rust_tests_exit=" + cargo.status);
if (failures.length) {
  for (const failure of failures) console.error("plugin_failure=" + failure);
  console.error("OMARCHY_GPUI_PLUGIN_PARITY_PENDING: " + failures.length + " checks need work");
  process.exitCode = 1;
} else {
  console.log("OMARCHY_GPUI_PLUGIN_PARITY_OK");
}
