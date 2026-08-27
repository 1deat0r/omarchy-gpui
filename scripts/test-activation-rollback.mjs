import { existsSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const launcher = path.join(projectRoot, "bin/omarchy-shell");
const binary = process.env.OMARCHY_GPUI_BIN || path.join(projectRoot, "target/debug/omarchy-gpui-shell");
const failures = [];

if (!existsSync(launcher) || !statSync(launcher).mode.toString(8).endsWith("755")) {
  failures.push("bin/omarchy-shell is missing or not executable");
}

const launcherSource = readFileSync(launcher, "utf8");
for (const marker of ["OMARCHY_GPUI_BIN", "target/debug/omarchy-gpui-shell", "-q"]) {
  if (!launcherSource.includes(marker)) failures.push("launcher omits " + marker);
}

const ping = spawnSync(launcher, ["-q", "shell", "ping"], {
  cwd: projectRoot,
  env: process.env,
  encoding: "utf8",
  timeout: 20000,
});
if (ping.status !== 0) {
  failures.push("launcher ping failed with " + ping.status + ": " + (ping.stderr || "").trim());
}

const help = spawnSync(binary, ["--help"], {
  cwd: projectRoot,
  env: process.env,
  encoding: "utf8",
  timeout: 20000,
});
if (help.status !== 0 || !help.stdout.includes("--smoke")) {
  failures.push("GPUI binary help/rollback entrypoint is unavailable");
}

const plan = readFileSync(path.join(projectRoot, "PLAN.md"), "utf8").toLowerCase();
for (const marker of ["rollback", "omarchy_gpui_bin", "activation"]) {
  if (!plan.includes(marker)) failures.push("PLAN.md does not document " + marker);
}

console.log("launcher_ping_exit=" + ping.status);
console.log("binary_help_exit=" + help.status);
if (failures.length) {
  for (const failure of failures) console.error("activation_failure=" + failure);
  console.error("OMARCHY_GPUI_ACTIVATION_ROLLBACK_PENDING: " + failures.length + " checks need work");
  process.exitCode = 1;
} else {
  console.log("OMARCHY_GPUI_ACTIVATION_ROLLBACK_OK");
}
