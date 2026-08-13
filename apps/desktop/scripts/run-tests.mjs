import { readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const packageJson = JSON.parse(
  await readFile(new URL("../package.json", import.meta.url), "utf8"),
);
const testScripts = Object.keys(packageJson.scripts ?? {})
  .filter((name) => name.startsWith("test:"))
  .sort();

if (testScripts.length === 0) {
  throw new Error("No test:* scripts are registered");
}

const npm = process.platform === "win32" ? "npm.cmd" : "npm";
for (const script of testScripts) {
  const result = spawnSync(npm, ["run", "--silent", script], {
    cwd: appRoot,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}

console.log(`WakeGPT Node contracts: ${testScripts.length}/${testScripts.length} passed`);
