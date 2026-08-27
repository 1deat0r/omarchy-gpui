import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const binary = process.env.OMARCHY_GPUI_BIN || path.join(projectRoot, "target/debug/omarchy-gpui-shell");
const failures = [];

function run(args) {
  const result = spawnSync(binary, args, {
    cwd: projectRoot,
    env: process.env,
    encoding: "utf8",
    timeout: 30000,
  });
  return {
    status: result.status,
    stdout: result.stdout || "",
    stderr: (result.stderr || "") + (result.error ? result.error.message : ""),
  };
}

if (!process.env.WAYLAND_DISPLAY) failures.push("WAYLAND_DISPLAY is not set");
if (!process.env.HYPRLAND_INSTANCE_SIGNATURE) failures.push("HYPRLAND_INSTANCE_SIGNATURE is not set");

const system = run(["--print-system"]);
if (system.status !== 0 || !system.stdout.includes("OMARCHY_GPUI_SYSTEM_RUNTIME_OK")) {
  failures.push("system probe failed: " + (system.stderr.trim() || system.stdout.trim()));
}

const jsonLine = system.stdout.match(/^system=(\{.*\})$/m)?.[1];
if (!jsonLine) {
  failures.push("system probe did not emit a JSON envelope");
} else {
  try {
    const value = JSON.parse(jsonLine);
    for (const key of [
      "hyprland", "audio", "network", "bluetooth", "battery", "media",
      "display", "power", "resources", "nightlight",
    ]) {
      if (!(key in value)) failures.push("system snapshot omits " + key);
    }
    if (value.hyprland?.available !== true) failures.push("Hyprland is not live");
    if (value.display?.available !== true) failures.push("display adapter is not live");
    for (const [key, adapter] of Object.entries(value)) {
      if (adapter && typeof adapter === "object" && !("error" in adapter)) {
        failures.push(key + " adapter omits its failure field");
      }
    }
  } catch (error) {
    failures.push("system JSON is invalid: " + error.message);
  }
}

const plugins = run(["--print-plugins"]);
if (plugins.status !== 0 || !plugins.stdout.includes("OMARCHY_GPUI_PLUGIN_RUNTIME_OK")) {
  failures.push("plugin probe failed: " + (plugins.stderr.trim() || plugins.stdout.trim()));
}

const cargo = spawnSync(
  "/home/mustbearnold/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin/cargo",
  ["test", "--offline", "system::tests"],
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
if (cargo.status !== 0) failures.push("system integration parser tests failed");

console.log("system_probe_exit=" + system.status);
console.log("plugin_probe_exit=" + plugins.status);
console.log("system_tests_exit=" + cargo.status);
if (failures.length) {
  for (const failure of failures) console.error("system_failure=" + failure);
  console.error("OMARCHY_GPUI_SYSTEM_PARITY_PENDING: " + failures.length + " checks need work");
  process.exitCode = 1;
} else {
  console.log("OMARCHY_GPUI_SYSTEM_PARITY_OK");
}
