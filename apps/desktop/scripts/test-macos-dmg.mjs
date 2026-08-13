import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const packager = fileURLToPath(new URL("./package-macos-dmg.mjs", import.meta.url));

if (process.platform !== "darwin") {
  console.log("macOS post-sign DMG contract: skipped outside macOS");
  process.exit(0);
}

const root = await mkdtemp(join(tmpdir(), "wakegpt-macos-dmg-test-"));
const app = join(root, "WakeGPT.app");
const executableDirectory = join(app, "Contents", "MacOS");
const executable = join(executableDirectory, "wakegpt-desktop");
const output = join(root, "WakeGPT_0.1.0_test.dmg");

const run = (arguments_, expectedStatus = 0) => {
  const result = spawnSync(process.execPath, [packager, ...arguments_], {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.equal(result.status, expectedStatus, result.stderr || result.stdout);
  return result;
};

try {
  await mkdir(executableDirectory, { recursive: true });
  const compiled = spawnSync("/usr/bin/clang", ["-x", "c", "-", "-o", executable], {
    input: "int main(void) { return 0; }\n",
    encoding: "utf8",
  });
  assert.equal(compiled.status, 0, compiled.stderr || compiled.stdout);
  await chmod(executable, 0o755);
  await writeFile(join(app, "Contents", "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>wakegpt-desktop</string>
<key>CFBundleIdentifier</key><string>com.wakegpt.desktop</string>
<key>CFBundleName</key><string>WakeGPT</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
</dict></plist>
`, "utf8");
  const signed = spawnSync("codesign", ["--force", "--deep", "--sign", "-", app], { encoding: "utf8" });
  assert.equal(signed.status, 0, signed.stderr || signed.stdout);

  const baseArguments = ["--app", app, "--output", output, "--volume-name", "WakeGPT Test"];
  const rejected = run(baseArguments, 1);
  assert.match(rejected.stderr, /Developer ID Application/u);

  const productionNamedOutput = join(root, "WakeGPT_0.1.0_aarch64.dmg");
  const productionNamed = run([
    "--app",
    app,
    "--output",
    productionNamedOutput,
    "--volume-name",
    "WakeGPT Test",
    "--allow-adhoc",
  ], 1);
  assert.match(productionNamed.stderr, /test-only output filename/u);

  const packaged = run([...baseArguments, "--allow-adhoc"]);
  const report = JSON.parse(packaged.stdout.trim());
  assert.equal(report.file, "WakeGPT_0.1.0_test.dmg");
  assert.match(report.sha256, /^[a-f0-9]{64}$/u);
  assert.equal(report.signature, "adhoc-test-only");
  run([...baseArguments, "--allow-adhoc"], 1);

  const linkedDirectory = join(root, "linked");
  await mkdir(linkedDirectory);
  await symlink(app, join(linkedDirectory, "WakeGPT.app"));
  const linkedOutput = join(root, "linked-test.dmg");
  const linked = run([
    "--app",
    join(linkedDirectory, "WakeGPT.app"),
    "--output",
    linkedOutput,
    "--volume-name",
    "WakeGPT Test",
    "--allow-adhoc",
  ], 1);
  assert.match(linked.stderr, /non-symlink directory/u);
} finally {
  await rm(root, { recursive: true, force: true });
}

console.log("macOS post-sign DMG contract: ok");
