import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const referenceRoot = process.env.OMARCHY_PATH || "/usr/share/omarchy";
const binary = process.env.OMARCHY_GPUI_BIN || path.join(projectRoot, "target/debug/omarchy-gpui-shell");

const referenceMatrix = {
  shell: [
    "ping", "applyTheme", "summon", "hide", "toggle", "call", "rescanPlugins",
    "reloadConfig", "toggleBarTransparency", "setPluginEnabled", "enablePlugin",
    "putBarWidget", "moveBarWidget", "setBarWidget", "listPlugins",
    "listShellConfig", "debugBarGeometry", "togglePanelAt",
  ],
  "image-selector": ["open", "preload", "cancel", "ping"],
  background: ["refresh", "set", "setInstant", "transition", "themeTransition"],
  lock: ["lock", "isLocked", "status", "preview", "hidePreview"],
  notifications: [
    "clear", "dismiss", "dismissAll", "dismissOne", "dndState", "invokeLast",
    "invokeAction", "isDnd", "ping", "setDnd", "showHistory", "toggleDnd",
  ],
  osd: ["close", "ping", "show", "state"],
  "omarchy.agents": ["open", "close", "show", "hide", "toggle", "refresh", "next"],
  "omarchy.indicators": ["refresh"],
  "omarchy.system-update": ["refresh", "clear"],
  "omarchy.bluetooth": ["open", "close", "show", "hide", "toggle", "toggleBluetooth"],
  "omarchy.clock": [
    "open", "close", "show", "hide", "toggle", "refresh", "cycleFormat", "toggleWeekStart",
  ],
  "omarchy.monitor": ["open", "close", "show", "hide", "toggle", "brightness", "state"],
  "omarchy.network": [
    "open", "close", "show", "hide", "toggle", "toggleNetwork", "showQr", "speedTest",
  ],
  "omarchy.power": ["open", "close", "show", "hide", "toggle", "togglePercentage"],
  idle: ["status", "debug", "enable", "disable", "toggle"],
  media: [
    "status", "playPause", "next", "previous", "play", "pause", "sourceNext",
    "sourcePrevious", "sourceSwitch", "sourceSwitchPrevious", "ping",
  ],
  nightlight: ["status", "refresh", "enable", "disable", "toggle"],
};

const failures = [];

function run(args, env, options = {}) {
  const result = spawnSync(binary, args, {
    cwd: projectRoot,
    env: { ...process.env, ...env },
    encoding: "utf8",
    timeout: options.timeout || 20000,
  });
  return {
    status: result.status,
    stdout: result.stdout || "",
    stderr: (result.stderr || "") + (result.error ? result.error.message : ""),
  };
}

const ipcSource = readFileSync(path.join(projectRoot, "src/ipc.rs"), "utf8");
const contract = run(["--print-contract"], {});
if (contract.status !== 0 || !contract.stdout.includes("OMARCHY_GPUI_CONTRACT_RUNTIME_OK")) {
  failures.push("contract probe failed: " + (contract.stderr.trim() || contract.stdout.trim()));
}

const contractMethods = new Set(
  (contract.stdout.match(/^ipc_methods=(.+)$/m)?.[1] || "").split(",").filter(Boolean),
);
for (const method of referenceMatrix.shell) {
  if (!contractMethods.has(method)) failures.push("shell contract omits " + method);
}

for (const [target, methods] of Object.entries(referenceMatrix)) {
  for (const method of methods) {
    if (!ipcSource.includes('"' + method + '"')) {
      failures.push("IPC source has no implementation marker for " + target + "." + method);
    }
  }
}

const isolatedHome = mkdtempSync(path.join(tmpdir(), "omarchy-gpui-ipc-"));
const isolatedRuntime = path.join(isolatedHome, "runtime");
const isolatedEnv = {
  HOME: isolatedHome,
  XDG_CONFIG_HOME: path.join(isolatedHome, ".config"),
  XDG_STATE_HOME: path.join(isolatedHome, ".local/state"),
  XDG_RUNTIME_DIR: isolatedRuntime,
  OMARCHY_PATH: referenceRoot,
  OMARCHY_GPUI_SOCKET: path.join(isolatedRuntime, "shell.sock"),
};

try {
  const safeCommands = [
    ["shell", "ping"],
    ["shell", "listPlugins"],
    ["shell", "listShellConfig"],
    ["shell", "debugBarGeometry"],
    ["image-selector", "ping"],
    ["shell", "call", "notifications", "ping"],
    ["shell", "call", "notifications", "dndState"],
    ["shell", "call", "idle", "status"],
    ["shell", "call", "media", "status"],
    ["shell", "call", "nightlight", "status"],
    ["shell", "call", "lock", "status"],
    ["shell", "call", "osd", "state"],
    ["shell", "call", "monitor", "state"],
    ["shell", "call", "network", "showQr"],
    ["shell", "call", "network", "speedTest"],
  ];
  for (const args of safeCommands) {
    const result = run(args, isolatedEnv);
    if (result.status !== 0) {
      failures.push(args.join(" ") + " exited " + result.status + ": " + result.stderr.trim());
    }
  }

  const before = run(["shell", "listShellConfig"], isolatedEnv);
  const toggle = run(["shell", "toggleBarTransparency"], isolatedEnv);
  if (toggle.status !== 0) {
    failures.push("toggleBarTransparency did not persist: " + toggle.stderr.trim());
  }
  const after = run(["shell", "listShellConfig"], isolatedEnv);
  if (before.stdout === after.stdout) failures.push("toggleBarTransparency produced no config change");

  const unknown = run(["shell", "call", "not-a-real-target", "ping"], isolatedEnv);
  if (unknown.status !== 0 || !unknown.stdout.includes("unknown")) {
    failures.push("unknown target did not follow the reference fail-open result");
  }
} finally {
  rmSync(isolatedHome, { recursive: true, force: true });
}

console.log("reference_ipc_targets=" + Object.keys(referenceMatrix).length);
console.log("reference_ipc_methods=" + Object.values(referenceMatrix).flat().length);
console.log("checked_safe_commands=19");
if (failures.length) {
  for (const failure of failures) console.error("ipc_failure=" + failure);
  console.error("OMARCHY_GPUI_IPC_PARITY_PENDING: " + failures.length + " checks need work");
  process.exitCode = 1;
} else {
  console.log("OMARCHY_GPUI_IPC_PARITY_OK");
}
