import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const descriptionPath = path.join(projectRoot, ".github/repository-description.txt");
const description = fs.readFileSync(descriptionPath, "utf8").trim();
if (!description) throw new Error("repository description is empty");

const remote = execFileSync("git", ["remote", "get-url", "origin"], {
  cwd: projectRoot,
  encoding: "utf8",
}).trim();
const match = remote.match(/github\.com[/:]([^/]+\/[^/]+?)(?:\.git)?$/);
if (!match) throw new Error(`origin is not a GitHub repository: ${remote}`);

execFileSync("gh", ["repo", "edit", match[1], "--description", description], {
  cwd: projectRoot,
  stdio: "inherit",
});
console.log(`OMARCHY_GPUI_GITHUB_DESCRIPTION_UPDATED: ${match[1]}`);
