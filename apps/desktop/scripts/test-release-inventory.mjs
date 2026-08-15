import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const generator = fileURLToPath(new URL("./generate-release-inventory.mjs", import.meta.url));
const outputDirectory = await mkdtemp(join(tmpdir(), "wakegpt-release-inventory-"));
const packageVersion = JSON.parse(await readFile(join(appRoot, "package.json"), "utf8")).version;
const sbomPath = join(outputDirectory, `wakegpt-${packageVersion}.cdx.json`);
const licensePath = join(outputDirectory, `wakegpt-${packageVersion}-licenses.json`);

const [projectLicense, cargoManifest, readme, notices, contributing] = await Promise.all([
  readFile(join(appRoot, "../..", "LICENSE"), "utf8"),
  readFile(join(appRoot, "src-tauri", "Cargo.toml"), "utf8"),
  readFile(join(appRoot, "../..", "README.md"), "utf8"),
  readFile(join(appRoot, "../..", "NOTICE.md"), "utf8"),
  readFile(join(appRoot, "../..", "CONTRIBUTING.md"), "utf8"),
]);
assert.equal(
  createHash("sha256").update(projectLicense).digest("hex"),
  "a60eea817514531668d7e00765731449fe14d059d3249e0bc93b36de45f759f2",
  "the root LICENSE must remain the canonical Apache-2.0 text",
);
assert.match(cargoManifest, /^license = "Apache-2\.0"$/mu);
assert.match(readme, /\[Apache License 2\.0\]\(LICENSE\)/u);
assert.match(notices, /Copyright 2026 WakeGPT Contributors/u);
assert.match(contributing, /Apache-2\.0 section 5 applies/u);

const generate = () => {
  const result = spawnSync(process.execPath, [generator, "--output-dir", outputDirectory], {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
  });
  assert.equal(result.status, 0, result.stderr || result.stdout);
};

try {
  generate();
  const firstSbom = await readFile(sbomPath, "utf8");
  const firstLicenses = await readFile(licensePath, "utf8");
  generate();
  assert.equal(await readFile(sbomPath, "utf8"), firstSbom, "SBOM output must be byte-reproducible");
  assert.equal(
    await readFile(licensePath, "utf8"),
    firstLicenses,
    "license inventory must be byte-reproducible",
  );

  const sbom = JSON.parse(firstSbom);
  const licenses = JSON.parse(firstLicenses);
  assert.equal(sbom.bomFormat, "CycloneDX");
  assert.equal(sbom.specVersion, "1.6");
  assert.equal(sbom.metadata.component.name, "wakegpt-desktop");
  assert.equal(sbom.metadata.component.version, packageVersion);
  assert.deepEqual(sbom.metadata.component.licenses, [{ license: { id: "Apache-2.0" } }]);
  assert.equal(licenses.project.version, packageVersion);
  assert.equal(licenses.project.licenseDecision, "Apache-2.0");
  assert.ok(sbom.components.length > 600, "the complete npm and Cargo lock graphs must be present");
  assert.equal(licenses.componentCount, sbom.components.length);
  assert.equal(licenses.components.length, sbom.components.length);
  assert.equal(new Set(sbom.components.map((component) => component["bom-ref"])).size, sbom.components.length);
  assert.equal(sbom.dependencies.length, sbom.components.length + 1);
  assert.ok(sbom.dependencies.every((dependency) => Array.isArray(dependency.dependsOn)));
  assert.ok(licenses.components.every((component) => component.license.length > 0));

  const requiredPurls = [
    "pkg:npm/%40tauri-apps/api@2.11.1",
    "pkg:npm/react@19.2.8",
    "pkg:cargo/tauri@2.11.5",
    "pkg:cargo/rusqlite@0.40.1",
    "pkg:cargo/objc2-app-kit@0.3.2",
  ];
  const componentPurls = new Set(sbom.components.map((component) => component.purl));
  for (const purl of requiredPurls) assert.ok(componentPurls.has(purl), `${purl} must be inventoried`);

  assert.doesNotMatch(firstSbom, /(?:\/Users\/|\/home\/|[A-Za-z]:\\Users\\)/u);
  assert.doesNotMatch(firstLicenses, /(?:\/Users\/|\/home\/|[A-Za-z]:\\Users\\)/u);
  assert.doesNotMatch(firstSbom, /"timestamp"\s*:/u, "dynamic timestamps would break reproducibility");
} finally {
  await rm(outputDirectory, { recursive: true, force: true });
}

console.log("release inventory contract: ok");
