import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { gzipSync } from "node:zlib";
import {
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  rm,
} from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import {
  archiveEvidence,
  archiveSingleRoot,
  assertCanonicalOnlyNeeded,
  assertNoStaleLicenseMappings,
  compareCodePoints,
  fetchBoundedHttps,
  installOutputDirectory,
  isLicenseEvidencePath,
  licenseTerms,
  parseTarGzip,
  selectVerifiedCargoArchive,
  sha256,
  verifyCargoSourceBinding,
  verifyNpmFamilyRelationship,
  verifySri,
} from "./license-bundle-core.mjs";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const generator = fileURLToPath(new URL("./generate-third-party-licenses.mjs", import.meta.url));
const temporaryRoot = await mkdtemp(join(tmpdir(), "wakegpt-third-party-licenses-"));

function tarHeader(path, size) {
  const header = Buffer.alloc(512);
  header.write(path, 0, 100, "utf8");
  header.write("0000644\0", 100, 8, "ascii");
  header.write("0000000\0", 108, 8, "ascii");
  header.write("0000000\0", 116, 8, "ascii");
  header.write(`${size.toString(8).padStart(11, "0")}\0`, 124, 12, "ascii");
  header.write("00000000000\0", 136, 12, "ascii");
  header.fill(0x20, 148, 156);
  header[156] = "0".charCodeAt(0);
  header.write("ustar\0", 257, 6, "ascii");
  header.write("00", 263, 2, "ascii");
  const checksum = header.reduce((sum, byte) => sum + byte, 0);
  header.write(`${checksum.toString(8).padStart(6, "0")}\0 `, 148, 8, "ascii");
  return header;
}

function tarGzip(entries) {
  const parts = [];
  for (const [path, value] of entries) {
    const data = Buffer.from(value);
    parts.push(tarHeader(path, data.length), data);
    const padding = (512 - (data.length % 512)) % 512;
    if (padding) parts.push(Buffer.alloc(padding));
  }
  parts.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(parts), { mtime: 0 });
}

function runGenerator(outputDirectory, expectedStatus = 0, environment = {}) {
  const result = spawnSync(process.execPath, [generator, "--output-dir", outputDirectory], {
    cwd: appRoot,
    encoding: "utf8",
    maxBuffer: 128 * 1024 * 1024,
    env: { ...process.env, ...environment },
  });
  assert.equal(result.status, expectedStatus, result.stderr || result.stdout);
  return result;
}

async function readGenerated(outputDirectory) {
  return {
    manifest: await readFile(join(outputDirectory, "wakegpt-0.1.0-third-party-licenses.json"), "utf8"),
    bundle: await readFile(join(outputDirectory, "THIRD-PARTY-LICENSES.txt"), "utf8"),
  };
}

try {
  const firstDirectory = join(temporaryRoot, "first");
  const secondDirectory = join(temporaryRoot, "second");
  runGenerator(firstDirectory, 0, { LANG: "C", LC_ALL: "C" });
  runGenerator(secondDirectory, 0, { LANG: "tr_TR.UTF-8", LC_ALL: "tr_TR.UTF-8" });
  const first = await readGenerated(firstDirectory);
  const second = await readGenerated(secondDirectory);
  assert.equal(second.manifest, first.manifest, "license manifests must be byte-reproducible");
  assert.equal(second.bundle, first.bundle, "license text bundles must be byte-reproducible");

  const manifest = JSON.parse(first.manifest);
  assert.equal(manifest.componentCount, 717);
  assert.equal(manifest.components.filter((item) => item.ecosystem === "npm").length, 229);
  assert.equal(manifest.components.filter((item) => item.ecosystem === "cargo").length, 488);
  assert.equal(manifest.components.length, manifest.componentCount);
  assert.equal(new Set(manifest.components.map((item) => item.purl)).size, manifest.componentCount);
  assert.equal(manifest.project.licenseDecision, "Apache-2.0");
  assert.match(first.bundle, /WakeGPT itself is licensed under Apache-2\.0/u);

  const packageLock = JSON.parse(await readFile(join(appRoot, "package-lock.json"), "utf8"));
  const expectedNpmIdentities = Object.entries(packageLock.packages)
    .filter(([path]) => path.startsWith("node_modules/"))
    .map(([path, metadata]) => {
      const marker = "node_modules/";
      const name = metadata.name ?? path.slice(path.lastIndexOf(marker) + marker.length);
      return `npm:${name}@${metadata.version}`;
    });
  const cargo = spawnSync(
    "cargo",
    ["metadata", "--locked", "--offline", "--format-version", "1"],
    { cwd: join(appRoot, "src-tauri"), encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
  );
  assert.equal(cargo.status, 0, cargo.stderr || cargo.stdout);
  const expectedCargoIdentities = JSON.parse(cargo.stdout).packages
    .filter((item) => item.source !== null)
    .map((item) => `cargo:${item.name}@${item.version}`);
  const expectedIdentities = [...expectedNpmIdentities, ...expectedCargoIdentities].sort(compareCodePoints);
  const generatedIdentities = manifest.components
    .map((item) => `${item.ecosystem}:${item.name}@${item.version}`)
    .sort(compareCodePoints);
  assert.deepEqual(generatedIdentities, expectedIdentities, "the generated component identities must equal both lock graphs");

  const textIds = new Set(manifest.texts.map((item) => item.id));
  assert.equal(textIds.size, manifest.textCount);
  assert.ok(manifest.texts.every((item) => item.sources.length > 0 && item.byteSize > 0));
  for (const component of manifest.components) {
    assert.ok(component.canonicalTextIds.length > 0, `${component.purl} needs canonical text`);
    assert.ok(
      component.evidenceTextIds.length > 0 || component.reviewedCanonicalOnly,
      `${component.purl} needs package or reviewed canonical-only evidence`,
    );
    for (const id of [...component.canonicalTextIds, ...component.evidenceTextIds]) {
      assert.ok(textIds.has(id), `${component.purl} references an unknown text`);
      assert.match(first.bundle, new RegExp(`^${id}$`, "mu"));
    }
  }
  const canonicalOnly = manifest.components.filter((item) => item.reviewedCanonicalOnly);
  assert.deepEqual(canonicalOnly.map((item) => `${item.name}@${item.version}`), ["selectors@0.36.1"]);
  assert.equal(canonicalOnly[0].reviewedCanonicalOnly.revision, "635e1a19d02960588a00e189bd4bd5bdb150ec3d");
  assert.equal(manifest.textCount, 337);
  assert.ok(manifest.components.filter((item) => item.upstreamVersionEvidence).every((item) => (
    ["archive-vcs", "git-tag", "manifest"].includes(item.upstreamVersionEvidence.kind)
  )));

  for (const output of [first.manifest, first.bundle]) {
    assert.doesNotMatch(output, /(?:\/Users\/|\/home\/|[A-Za-z]:\\Users\\)/u);
    assert.doesNotMatch(output, /-----BEGIN (?:OPENSSH |RSA |EC )?PRIVATE KEY-----/u);
  }
  assert.doesNotMatch(first.manifest, /"(?:generatedAt|timestamp)"\s*:/u);
  const evidencePaths = manifest.texts.flatMap((item) => item.sources.map((source) => source.path).filter(Boolean));
  assert.ok(!evidencePaths.some((path) => /(?:copying\.rs|copyright\.mjs(?:\.map)?)/u.test(path)));

  runGenerator(firstDirectory, 1);
  assert.deepEqual(await readGenerated(firstDirectory), first, "a collision must preserve prior output");

  const knownLicenses = new Set(["MIT", "Apache-2.0"]);
  assert.throws(
    () => licenseTerms("Unknown-1.0", knownLicenses, new Set()),
    /No pinned canonical text/u,
  );
  assert.throws(
    () => verifySri(Buffer.from("archive"), `sha512-${Buffer.alloc(64).toString("base64")}`),
    /SRI mismatch/u,
  );
  assert.throws(
    () => selectVerifiedCargoArchive("crate@1.0.0", "0".repeat(64), []),
    /unavailable/u,
  );
  assert.throws(
    () => selectVerifiedCargoArchive("crate@1.0.0", "0".repeat(64), [Buffer.from("tampered")]),
    /checksum mismatch/u,
  );
  const crate = Buffer.from("verified crate");
  assert.equal(selectVerifiedCargoArchive("crate@1.0.0", sha256(crate), [crate]), crate);

  const familyParent = {
    packageMetadata: {
      name: "parent",
      version: "1.0.0",
      license: "MIT",
      optionalDependencies: { "@family/child": "1.0.0" },
    },
    evidence: [{ path: "LICENSE", text: "parent\n" }],
  };
  const familyChild = {
    packageMetadata: { name: "@family/child", version: "1.0.0", license: "MIT" },
    evidence: [],
  };
  assert.deepEqual(
    verifyNpmFamilyRelationship({
      name: "@family/child",
      version: "1.0.0",
      license: "MIT",
      parentName: "parent",
      child: familyChild,
      parent: familyParent,
    }),
    { evidence: familyParent.evidence, evidenceOwner: "parent" },
  );
  assert.throws(
    () => verifyNpmFamilyRelationship({
      name: "@family/forged",
      version: "1.0.0",
      license: "MIT",
      parentName: "parent",
      child: familyChild,
      parent: familyParent,
    }),
    /identity/u,
  );
  assert.throws(
    () => verifyNpmFamilyRelationship({
      name: "@family/child",
      version: "2.0.0",
      license: "MIT",
      parentName: "parent",
      child: familyChild,
      parent: familyParent,
    }),
    /identity|does not pin/u,
  );

  const bindingMetadata = {
    name: "crate",
    version: "1.0.0",
    license: "MIT",
    repository: "https://github.com/example/crate",
  };
  const bindingEntry = {
    repository: bindingMetadata.repository,
    revision: "1".repeat(40),
    versionEvidence: { kind: "archive-vcs" },
  };
  const noFetch = async () => { throw new Error("unexpected fetch"); };
  const noTag = async () => { throw new Error("unexpected tag lookup"); };
  assert.deepEqual(await verifyCargoSourceBinding({
    packageId: "crate@1.0.0",
    metadata: bindingMetadata,
    archiveRevision: "1".repeat(40),
    entry: bindingEntry,
    fetchManifest: noFetch,
    resolveGitTag: noTag,
  }), { kind: "archive-vcs", revision: "1".repeat(40) });
  await assert.rejects(
    verifyCargoSourceBinding({
      packageId: "crate@1.0.0",
      metadata: { ...bindingMetadata, repository: "https://github.com/example/other" },
      archiveRevision: "1".repeat(40),
      entry: bindingEntry,
      fetchManifest: noFetch,
      resolveGitTag: noTag,
    }),
    /repository mismatch/u,
  );
  await assert.rejects(
    verifyCargoSourceBinding({
      packageId: "crate@1.0.0",
      metadata: bindingMetadata,
      archiveRevision: "2".repeat(40),
      entry: bindingEntry,
      fetchManifest: noFetch,
      resolveGitTag: noTag,
    }),
    /revision mismatch/u,
  );
  await assert.rejects(
    verifyCargoSourceBinding({
      packageId: "crate@1.0.0",
      metadata: bindingMetadata,
      archiveRevision: null,
      entry: bindingEntry,
      requireRevision: true,
      fetchManifest: noFetch,
      resolveGitTag: noTag,
    }),
    /lacks required VCS/u,
  );
  const tagEntry = {
    ...bindingEntry,
    versionEvidence: {
      kind: "git-tag",
      tag: "v1.0.0",
      tagObject: "3".repeat(40),
      version: "1.0.0",
    },
  };
  assert.deepEqual(await verifyCargoSourceBinding({
    packageId: "crate@1.0.0",
    metadata: bindingMetadata,
    archiveRevision: null,
    entry: tagEntry,
    fetchManifest: noFetch,
    resolveGitTag: async () => ({ tagObject: "3".repeat(40), revision: "1".repeat(40) }),
  }), { kind: "git-tag", tag: "v1.0.0", tagObject: "3".repeat(40) });
  await assert.rejects(
    verifyCargoSourceBinding({
      packageId: "crate@1.0.0",
      metadata: bindingMetadata,
      archiveRevision: null,
      entry: tagEntry,
      fetchManifest: noFetch,
      resolveGitTag: async () => ({ tagObject: "4".repeat(40), revision: "1".repeat(40) }),
    }),
    /does not resolve/u,
  );
  const manifestBytes = Buffer.from('[package]\nname = "crate"\nversion = "1.0.0"\nlicense = "MIT"\n');
  const manifestEntry = {
    ...bindingEntry,
    versionEvidence: {
      kind: "manifest",
      path: "Cargo.toml",
      sha256: sha256(manifestBytes),
      version: "1.0.0",
    },
  };
  assert.equal((await verifyCargoSourceBinding({
    packageId: "crate@1.0.0",
    metadata: bindingMetadata,
    archiveRevision: null,
    entry: manifestEntry,
    fetchManifest: async () => manifestBytes,
    resolveGitTag: noTag,
  })).kind, "manifest");
  await assert.rejects(
    verifyCargoSourceBinding({
      packageId: "crate@1.0.0",
      metadata: bindingMetadata,
      archiveRevision: null,
      entry: manifestEntry,
      fetchManifest: async () => Buffer.from('[package]\nname = "other"\nversion = "1.0.0"\nlicense = "MIT"\n'),
      resolveGitTag: noTag,
    }),
    /checksum mismatch|identity/u,
  );

  assert.throws(
    () => assertNoStaleLicenseMappings({
      npmFamilies: [{ prefix: "@unused/" }],
      cargoRemoteEvidence: [],
      cargoCanonicalOnly: [],
    }, { usedNpmFamilyPrefixes: new Set(), cargoPackageIds: new Set() }),
    /Stale npm family/u,
  );
  assert.throws(
    () => assertNoStaleLicenseMappings({
      npmFamilies: [],
      cargoRemoteEvidence: [{ packages: ["missing@1.0.0"] }],
      cargoCanonicalOnly: [],
    }, { usedNpmFamilyPrefixes: new Set(), cargoPackageIds: new Set() }),
    /Stale Cargo/u,
  );
  assert.throws(
    () => assertNoStaleLicenseMappings({
      npmFamilies: [],
      cargoRemoteEvidence: [],
      cargoCanonicalOnly: [{ package: "missing@1.0.0" }],
    }, { usedNpmFamilyPrefixes: new Set(), cargoPackageIds: new Set() }),
    /Stale canonical-only/u,
  );
  assert.doesNotThrow(() => assertCanonicalOnlyNeeded(
    "missing-text@1.0.0",
    [],
    { package: "missing-text@1.0.0" },
  ));
  assert.throws(
    () => assertCanonicalOnlyNeeded(
      "now-complete@1.0.0",
      [{ path: "LICENSE", text: "license\n" }],
      { package: "now-complete@1.0.0" },
    ),
    /Stale canonical-only mapping now has archive evidence/u,
  );

  assert.throws(
    () => parseTarGzip(tarGzip([["package/../escape", "bad"]])),
    /unsafe path/u,
  );
  const duplicateEntries = parseTarGzip(tarGzip([
    ["package/LICENSE", "one"],
    ["package/LICENSE", "two"],
  ]));
  assert.throws(() => archiveEvidence(duplicateEntries, "package"), /duplicate file path/u);
  const rooted = parseTarGzip(tarGzip([
    ["babel__core/package.json", "{}"],
    ["babel__core/LICENSE.MIT", "MIT\n"],
  ]));
  assert.equal(archiveSingleRoot(rooted, "package.json"), "babel__core");
  assert.equal(archiveEvidence(rooted, "babel__core").evidence.length, 1);
  for (const path of [
    "LICENSE",
    "LICENSE-MIT",
    "license-apache-2.0",
    "LICENSE.BSD-3-Clause",
    "COPYRIGHT-RUST.txt",
    "vendor/dbus/cmake/modules/COPYING-CMAKE-SCRIPTS",
    "NOTICE.md",
    "AUTHORS",
  ]) {
    assert.equal(isLicenseEvidencePath(path), true, `${path} must be accepted as reviewed text evidence`);
  }
  for (const path of [
    "src/copying.rs",
    "dist/copyright.mjs",
    "dist/copyright.mjs.map",
    "NOTICE.class",
    "COPYING.sh",
    "LICENSE.exe",
    "AUTHORS.json",
    "copyright.css",
    "NOTICE.swift",
    "LICENSE-unknown",
  ]) {
    assert.equal(isLicenseEvidencePath(path), false, `${path} must not satisfy the license evidence gate`);
  }
  assert.throws(() => parseTarGzip(tarGzip([["package/a", "a"]]), { maxArchiveBytes: 1 }), /size/u);

  await assert.rejects(
    fetchBoundedHttps("https://example.invalid/license", {
      allowedHosts: ["raw.githubusercontent.com"],
      fetchImpl: async () => { throw new Error("must not run"); },
    }),
    /explicitly allowed/u,
  );
  await assert.rejects(
    fetchBoundedHttps("https://raw.githubusercontent.com/license", {
      allowedHosts: ["raw.githubusercontent.com"],
      timeoutMs: 5,
      fetchImpl: async (_url, { signal }) => {
        await new Promise((resolve) => setTimeout(resolve, 20));
        if (signal.aborted) throw signal.reason;
        throw new Error("timeout signal did not abort the request");
      },
    }),
    /timed out/u,
  );
  await assert.rejects(
    fetchBoundedHttps("https://raw.githubusercontent.com/license", {
      allowedHosts: ["raw.githubusercontent.com"],
      maxBytes: 3,
      fetchImpl: async () => new Response("four", { headers: { "content-length": "4" } }),
    }),
    /allowed size/u,
  );

  const transaction = join(temporaryRoot, "transaction");
  await installOutputDirectory(transaction, [["LICENSE.txt", "license\n"]]);
  assert.equal(await readFile(join(transaction, "LICENSE.txt"), "utf8"), "license\n");
  await assert.rejects(
    installOutputDirectory(transaction, [["LICENSE.txt", "replacement\n"]]),
    /already exists/u,
  );
  assert.equal(await readFile(join(transaction, "LICENSE.txt"), "utf8"), "license\n");
  await assert.rejects(
    installOutputDirectory(join(temporaryRoot, "bad-names"), [
      ["LICENSE.txt", "one"],
      ["LICENSE.txt", "two"],
    ]),
    /unique basenames/u,
  );
  const failedTransaction = join(temporaryRoot, "failed-transaction");
  await assert.rejects(
    installOutputDirectory(failedTransaction, [
      ["FIRST.txt", "first\n"],
      ["SECOND.txt", Symbol("invalid")],
    ]),
  );
  await assert.rejects(lstat(failedTransaction), { code: "ENOENT" });
  const lockedTransaction = join(temporaryRoot, "locked-transaction");
  await mkdir(`${lockedTransaction}.lock`);
  await assert.rejects(
    installOutputDirectory(lockedTransaction, [["LICENSE.txt", "license\n"]]),
    /already locked/u,
  );

  const alteredTar = Buffer.from(tarGzip([["package/LICENSE", "text"]]));
  const unpacked = Buffer.from(await import("node:zlib").then(({ gunzipSync }) => gunzipSync(alteredTar)));
  unpacked[0] ^= 1;
  assert.throws(() => parseTarGzip(gzipSync(unpacked, { mtime: 0 })), /checksum mismatch/u);

  const expectedSha512 = createHash("sha512").update("archive").digest("base64");
  assert.equal(verifySri(Buffer.from("archive"), `sha512-${expectedSha512}`).algorithm, "sha512");
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}

console.log("third-party license bundle contract: ok");
