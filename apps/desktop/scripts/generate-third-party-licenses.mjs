import { spawnSync } from "node:child_process";
import {
  lstat,
  readFile,
  readdir,
} from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  archiveEvidence,
  archiveSingleRoot,
  assertCanonicalOnlyNeeded,
  assertNoStaleLicenseMappings,
  assertExactPackageIdentity,
  compareCodePoints,
  fetchBoundedHttps,
  installOutputDirectory,
  licenseTerms,
  normalizedText,
  parseTarGzip,
  selectVerifiedCargoArchive,
  sha256,
  verifyCargoSourceBinding,
  verifyNpmFamilyRelationship,
  verifySri,
} from "./license-bundle-core.mjs";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const workspaceRoot = resolve(appRoot, "../..");
const cargoRoot = join(appRoot, "src-tauri");
const sourcePolicyPath = fileURLToPath(new URL("./license-sources.json", import.meta.url));
const MAX_ARCHIVE_BYTES = 128 * 1024 * 1024;
const MAX_REMOTE_TEXT_BYTES = 4 * 1024 * 1024;

function parseArguments(arguments_) {
  let outputDirectory = join(workspaceRoot, "release-output", "supply-chain", "third-party-licenses");
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "--output-dir") {
      const value = arguments_[index + 1];
      if (!value) throw new Error("--output-dir requires a path");
      outputDirectory = resolve(value);
      index += 1;
    } else {
      throw new Error(`Unknown argument: ${argument}`);
    }
  }
  return { outputDirectory };
}

function npmNameFromPath(packagePath) {
  const marker = "node_modules/";
  const index = packagePath.lastIndexOf(marker);
  if (index < 0) throw new Error(`Invalid package-lock path: ${packagePath}`);
  return packagePath.slice(index + marker.length);
}

function npmPurl(name, version) {
  if (!name.startsWith("@")) return `pkg:npm/${encodeURIComponent(name)}@${encodeURIComponent(version)}`;
  const slash = name.indexOf("/");
  if (slash < 0) throw new Error(`Invalid scoped npm package: ${name}`);
  return `pkg:npm/${encodeURIComponent(name.slice(0, slash))}/${encodeURIComponent(name.slice(slash + 1))}@${encodeURIComponent(version)}`;
}

function cargoPurl(name, version) {
  return `pkg:cargo/${encodeURIComponent(name)}@${encodeURIComponent(version)}`;
}

function cargoLockPackages(source) {
  const packages = new Map();
  for (const block of source.split(/^\[\[package\]\]\s*$/mu).slice(1)) {
    const read = (key) => block.match(new RegExp(`^${key} = "([^"]+)"$`, "mu"))?.[1] ?? null;
    const name = read("name");
    const version = read("version");
    const registry = read("source");
    const checksum = read("checksum");
    if (!name || !version || !registry) continue;
    if (registry !== "registry+https://github.com/rust-lang/crates.io-index" || !checksum) {
      throw new Error(`Unsupported Cargo source for ${name}@${version}`);
    }
    const id = `${name}@${version}`;
    if (packages.has(id)) throw new Error(`Duplicate Cargo package identity: ${id}`);
    packages.set(id, { checksum, registry });
  }
  return packages;
}

function canonicalVersion(policy) {
  const match = policy.canonical.version.match(/^SPDX License List ([0-9]+\.[0-9]+\.[0-9]+)$/u);
  if (!match) throw new Error("Canonical license policy must pin one SPDX License List version");
  return match[1];
}

function parsePackageIdentity(source, label) {
  let metadata;
  try {
    metadata = JSON.parse(normalizedText(source));
  } catch {
    throw new Error(`${label} contains invalid package metadata`);
  }
  return metadata;
}

function validateSourcePolicy(policy) {
  if (policy.schemaVersion !== 1) throw new Error("Unsupported license source policy schema");
  const remotePackages = new Set();
  for (const entry of policy.cargoRemoteEvidence) {
    if (!/^https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u.test(entry.repository)) {
      throw new Error("Cargo evidence repository must be an exact public GitHub repository");
    }
    if (!/^[0-9a-f]{40}$/u.test(entry.revision) || !entry.files.length) {
      throw new Error("Cargo evidence must pin a full Git revision and at least one file");
    }
    const validateVersionEvidence = (versionEvidence, packageId) => {
      const expectedVersion = packageId.split("@").at(-1);
      if (
        !["archive-vcs", "git-tag", "manifest"].includes(versionEvidence.kind)
        || (versionEvidence.kind === "archive-vcs" && Object.keys(versionEvidence).length !== 1)
        || (versionEvidence.kind === "git-tag"
          && (!/^[A-Za-z0-9._/-]+$/u.test(versionEvidence.tag)
            || !/^[0-9a-f]{40}$/u.test(versionEvidence.tagObject)
            || versionEvidence.version !== expectedVersion))
        || (versionEvidence.kind === "manifest"
          && (!/^[A-Za-z0-9._/-]+$/u.test(versionEvidence.path)
            || versionEvidence.path.includes("..")
            || !/^[0-9a-f]{64}$/u.test(versionEvidence.sha256)
            || versionEvidence.version !== expectedVersion))
      ) {
        throw new Error(`Cargo version evidence is invalid: ${packageId}`);
      }
    };
    if (entry.versionEvidence && entry.packageVersionEvidence) {
      throw new Error("Cargo evidence may use shared or per-package version evidence, not both");
    }
    const perPackageEvidence = entry.packageVersionEvidence ?? {};
    if (entry.packageVersionEvidence
      && (Object.keys(perPackageEvidence).length !== entry.packages.length
        || Object.keys(perPackageEvidence).some((packageId) => !entry.packages.includes(packageId)))) {
      throw new Error("Cargo per-package version evidence must cover the exact package set");
    }
    for (const file of entry.files) {
      if (!/^[A-Za-z0-9._/-]+$/u.test(file.path) || file.path.includes("..") || !/^[0-9a-f]{64}$/u.test(file.sha256)) {
        throw new Error("Cargo evidence file path or digest is invalid");
      }
    }
    for (const packageId of entry.packages) {
      if (remotePackages.has(packageId)) throw new Error(`Duplicate remote evidence mapping: ${packageId}`);
      validateVersionEvidence(perPackageEvidence[packageId] ?? entry.versionEvidence ?? { kind: "archive-vcs" }, packageId);
      remotePackages.add(packageId);
    }
  }
  const canonicalOnly = new Set();
  for (const entry of policy.cargoCanonicalOnly) {
    if (canonicalOnly.has(entry.package) || remotePackages.has(entry.package)) {
      throw new Error(`Duplicate Cargo fallback mapping: ${entry.package}`);
    }
    if (!/^https:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u.test(entry.repository)
      || !/^[0-9a-f]{40}$/u.test(entry.revision)
      || typeof entry.reason !== "string"
      || entry.reason.length < 20) {
      throw new Error(`Invalid canonical-only Cargo mapping: ${entry.package}`);
    }
    canonicalOnly.add(entry.package);
  }
  const prefixes = new Set();
  for (const family of policy.npmFamilies) {
    if (!family.prefix || prefixes.has(family.prefix) || !family.parent || family.versionMatch !== true) {
      throw new Error("Invalid npm family license mapping");
    }
    prefixes.add(family.prefix);
  }
  canonicalVersion(policy);
}

async function boundedFetch(url, expectedSha256 = null) {
  const parsed = new URL(url);
  const limit = parsed.hostname === "registry.npmjs.org" ? MAX_ARCHIVE_BYTES : MAX_REMOTE_TEXT_BYTES;
  return fetchBoundedHttps(parsed, {
    allowedHosts: ["raw.githubusercontent.com", "registry.npmjs.org"],
    expectedSha256,
    maxBytes: limit,
  });
}

async function assertOutputDirectoryAbsent(outputDirectory) {
  try {
    await lstat(outputDirectory);
    throw new Error(`Output directory already exists: ${basename(outputDirectory)}`);
  } catch (error) {
    if (error?.code !== "ENOENT") throw error;
  }
}

const { outputDirectory } = parseArguments(process.argv.slice(2));
await assertOutputDirectoryAbsent(outputDirectory);
const [packageJson, packageLock, cargoLock, policy] = await Promise.all([
  readFile(join(appRoot, "package.json"), "utf8").then(JSON.parse),
  readFile(join(appRoot, "package-lock.json"), "utf8").then(JSON.parse),
  readFile(join(cargoRoot, "Cargo.lock"), "utf8"),
  readFile(sourcePolicyPath, "utf8").then(JSON.parse),
]);
if (packageJson.license !== "Apache-2.0" || packageLock.packages?.[""]?.license !== packageJson.license) {
  throw new Error("WakeGPT project license metadata must consistently declare Apache-2.0");
}
validateSourcePolicy(policy);

const cargoResult = spawnSync(
  "cargo",
  ["metadata", "--locked", "--offline", "--format-version", "1"],
  { cwd: cargoRoot, encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
);
if (cargoResult.error) throw cargoResult.error;
if (cargoResult.status !== 0) throw new Error(cargoResult.stderr.trim() || "cargo metadata failed");
const cargoMetadata = JSON.parse(cargoResult.stdout);
const cargoChecksums = cargoLockPackages(cargoLock);

const knownLicenses = new Set(Object.keys(policy.canonical.licenses));
const knownExceptions = new Set(Object.keys(policy.canonical.exceptions));
const spdxVersion = canonicalVersion(policy);
const textById = new Map();
const canonicalSourceCache = new Map();

function registerText(text, source) {
  const normalized = normalizedText(Buffer.from(text, "utf8"));
  const digest = sha256(normalized);
  const id = `sha256:${digest}`;
  const current = textById.get(id);
  if (current && current.text !== normalized) throw new Error("License text digest collision");
  const record = current ?? { id, sha256: digest, byteSize: Buffer.byteLength(normalized), text: normalized, sources: [] };
  const sourceKey = JSON.stringify(source);
  if (!record.sources.some((item) => JSON.stringify(item) === sourceKey)) record.sources.push(source);
  textById.set(id, record);
  return id;
}

async function canonicalTextIds(terms) {
  const ids = [];
  for (const license of terms.licenses) {
    const evidence = policy.canonical.licenses[license];
    const url = `https://raw.githubusercontent.com/spdx/license-list-data/v${spdxVersion}/text/${license}.txt`;
    const cacheKey = `${url}:${evidence.sha256}`;
    let bytes = canonicalSourceCache.get(cacheKey);
    if (!bytes) {
      bytes = await boundedFetch(url, evidence.sha256);
      canonicalSourceCache.set(cacheKey, bytes);
    }
    ids.push(registerText(normalizedText(bytes), {
      kind: "canonical-license",
      license,
      version: policy.canonical.version,
      url,
      sourceSha256: evidence.sha256,
    }));
  }
  for (const exception of terms.exceptions) {
    const evidence = policy.canonical.exceptions[exception];
    const url = `https://raw.githubusercontent.com/spdx/license-list-data/v${spdxVersion}/text/${exception}.txt`;
    const cacheKey = `${url}:${evidence.sha256}`;
    let bytes = canonicalSourceCache.get(cacheKey);
    if (!bytes) {
      bytes = await boundedFetch(url, evidence.sha256);
      canonicalSourceCache.set(cacheKey, bytes);
    }
    ids.push(registerText(normalizedText(bytes), {
      kind: "canonical-exception",
      exception,
      version: policy.canonical.version,
      url,
      sourceSha256: evidence.sha256,
    }));
  }
  return [...new Set(ids)].sort(compareCodePoints);
}

const npmEntries = Object.entries(packageLock.packages)
  .filter(([packagePath]) => packagePath.startsWith("node_modules/"))
  .sort(([left], [right]) => compareCodePoints(left, right));
const npmByPath = new Map(npmEntries);
const npmArchiveCache = new Map();

async function npmArchive(packagePath, metadata) {
  if (npmArchiveCache.has(packagePath)) return npmArchiveCache.get(packagePath);
  const name = metadata.name ?? npmNameFromPath(packagePath);
  if (!metadata.version || !metadata.resolved || !metadata.integrity) {
    throw new Error(`Incomplete package-lock evidence for ${name}`);
  }
  const url = new URL(metadata.resolved);
  if (url.protocol !== "https:" || url.hostname !== "registry.npmjs.org" || url.username || url.password) {
    throw new Error(`npm archive must use the official credential-free registry: ${name}`);
  }
  const bytes = await boundedFetch(url.href);
  const sri = verifySri(bytes, metadata.integrity);
  const entries = parseTarGzip(bytes);
  const parsed = archiveEvidence(entries, archiveSingleRoot(entries, "package.json"));
  const packageMetadata = parsePackageIdentity(parsed.files.get("package.json"), `npm archive ${name}`);
  assertExactPackageIdentity(packageMetadata, {
    name,
    version: metadata.version,
    license: metadata.license,
  }, `npm archive ${name}`);
  const result = { ...parsed, packageMetadata, name, version: metadata.version, url: url.href, sri };
  npmArchiveCache.set(packagePath, result);
  return result;
}

const components = [];
const usedNpmFamilyPrefixes = new Set();
for (const [packagePath, metadata] of npmEntries) {
  const name = metadata.name ?? npmNameFromPath(packagePath);
  const version = metadata.version;
  if (!version) throw new Error(`${name} is missing a version`);
  const terms = licenseTerms(metadata.license, knownLicenses, knownExceptions);
  let evidence;
  let evidenceSource;
  const family = policy.npmFamilies.find((item) => name.startsWith(item.prefix));
  if (family) {
    usedNpmFamilyPrefixes.add(family.prefix);
    const parentPath = `node_modules/${family.parent}`;
    const parentMetadata = npmByPath.get(parentPath);
    if (!parentMetadata) throw new Error(`npm family parent is absent: ${family.parent}`);
    const parentName = parentMetadata.name ?? npmNameFromPath(parentPath);
    if (parentName !== family.parent
      || parentMetadata.version !== version
      || parentMetadata.license !== metadata.license) {
      throw new Error(`npm family identity or license mismatch: ${name}`);
    }
    const childArchive = await npmArchive(packagePath, metadata);
    const parentArchive = await npmArchive(parentPath, parentMetadata);
    const relationship = verifyNpmFamilyRelationship({
      name,
      version,
      license: metadata.license,
      parentName: family.parent,
      child: childArchive,
      parent: parentArchive,
    });
    evidence = relationship.evidence;
    evidenceSource = {
      kind: "npm-parent-archive",
      package: `${family.parent}@${version}`,
      childPackage: `${name}@${version}`,
      childUrl: childArchive.url,
      childIntegrityAlgorithm: childArchive.sri.algorithm,
      childIntegrityHex: childArchive.sri.hex,
      url: parentArchive.url,
      integrityAlgorithm: parentArchive.sri.algorithm,
      integrityHex: parentArchive.sri.hex,
      evidenceOwner: relationship.evidenceOwner,
    };
  } else {
    const archive = await npmArchive(packagePath, metadata);
    evidence = archive.evidence;
    evidenceSource = {
      kind: "npm-archive",
      package: `${name}@${version}`,
      url: archive.url,
      integrityAlgorithm: archive.sri.algorithm,
      integrityHex: archive.sri.hex,
    };
  }
  if (!evidence.length) throw new Error(`npm package has no license or notice evidence: ${name}@${version}`);
  const evidenceTextIds = evidence.map((item) => registerText(item.text, { ...evidenceSource, path: item.path }));
  components.push({
    ecosystem: "npm",
    name,
    version,
    purl: npmPurl(name, version),
    scope: metadata.dev === true ? "development" : "production",
    licenseExpression: metadata.license,
    normalizedLicenseExpression: terms.normalized,
    canonicalTextIds: await canonicalTextIds(terms),
    evidenceTextIds: [...new Set(evidenceTextIds)].sort(compareCodePoints),
  });
}

const cargoHome = resolve(process.env.CARGO_HOME || join(homedir(), ".cargo"));
const cacheRoot = join(cargoHome, "registry", "cache");
const cacheDirectories = (await readdir(cacheRoot, { withFileTypes: true }))
  .filter((entry) => entry.isDirectory() && !entry.isSymbolicLink())
  .map((entry) => entry.name)
  .sort(compareCodePoints);
const remoteByPackage = new Map(policy.cargoRemoteEvidence.flatMap((entry) => entry.packages.map((id) => [id, {
  ...entry,
  versionEvidence: entry.packageVersionEvidence?.[id] ?? entry.versionEvidence,
  packageVersionEvidence: undefined,
}])));
const canonicalOnlyByPackage = new Map(policy.cargoCanonicalOnly.map((entry) => [entry.package, entry]));
const remoteTextCache = new Map();

async function cargoArchive(packageId, name, version, checksum) {
  const filename = `${name}-${version}.crate`;
  const matches = [];
  for (const directory of cacheDirectories) {
    const path = join(cacheRoot, directory, filename);
    try {
      const info = await lstat(path);
      if (!info.isFile() || info.isSymbolicLink() || info.size <= 0 || info.size > MAX_ARCHIVE_BYTES) continue;
      const bytes = await readFile(path);
      matches.push(bytes);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
  }
  return archiveEvidence(
    parseTarGzip(selectVerifiedCargoArchive(packageId, checksum, matches)),
    `${name}-${version}`,
  );
}

function cargoVcsRevision(archive, packageId) {
  const vcsInfo = archive.files.get(".cargo_vcs_info.json");
  if (!vcsInfo) return null;
  let revision;
  try {
    revision = JSON.parse(normalizedText(vcsInfo)).git?.sha1 ?? null;
  } catch {
    throw new Error(`Cargo archive has invalid VCS metadata: ${packageId}`);
  }
  if (!/^[0-9a-f]{40}$/u.test(revision ?? "")) {
    throw new Error(`Cargo archive has invalid VCS revision: ${packageId}`);
  }
  return revision;
}

function resolveGitTag(repository, tag) {
  const arguments_ = ["ls-remote", "--tags", `${repository}.git`, `refs/tags/${tag}`, `refs/tags/${tag}^{}`];
  let result = spawnSync("git", arguments_, {
    encoding: "utf8",
    timeout: 60_000,
    maxBuffer: 1024 * 1024,
  });
  if (!result.error && result.status !== 0) {
    result = spawnSync("git", arguments_, {
      encoding: "utf8",
      timeout: 60_000,
      maxBuffer: 1024 * 1024,
    });
  }
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const detail = result.signal ? ` (${result.signal})` : "";
    throw new Error(`Unable to resolve reviewed Cargo tag: ${tag}${detail}`);
  }
  const refs = new Map(result.stdout.trim().split("\n").filter(Boolean).map((line) => {
    const [digest, ref] = line.split(/\s+/u);
    return [ref, digest];
  }));
  const tagObject = refs.get(`refs/tags/${tag}`);
  const revision = refs.get(`refs/tags/${tag}^{}`) ?? tagObject;
  if (!/^[0-9a-f]{40}$/u.test(tagObject ?? "") || !/^[0-9a-f]{40}$/u.test(revision ?? "")) {
    throw new Error(`Reviewed Cargo tag is missing or invalid: ${tag}`);
  }
  return { tagObject, revision };
}

async function verifyCargoSource(packageId, metadata, archive, entry, options = {}) {
  const [owner, repositoryName] = entry.repository.slice("https://github.com/".length).split("/");
  return verifyCargoSourceBinding({
    packageId,
    metadata,
    archiveRevision: cargoVcsRevision(archive, packageId),
    entry,
    ...options,
    resolveGitTag,
    fetchManifest: (_source, versionEvidence) => boundedFetch(
      `https://raw.githubusercontent.com/${owner}/${repositoryName}/${entry.revision}/${versionEvidence.path}`,
      versionEvidence.sha256,
    ),
  });
}

async function remoteEvidence(packageId, metadata, archive, entry) {
  const versionEvidence = await verifyCargoSource(packageId, metadata, archive, entry);
  const [owner, repositoryName] = entry.repository.slice("https://github.com/".length).split("/");
  const evidence = [];
  for (const file of entry.files) {
    const url = `https://raw.githubusercontent.com/${owner}/${repositoryName}/${entry.revision}/${file.path}`;
    const cacheKey = `${url}:${file.sha256}`;
    let text = remoteTextCache.get(cacheKey);
    if (!text) {
      text = normalizedText(await boundedFetch(url, file.sha256));
      remoteTextCache.set(cacheKey, text);
    }
    evidence.push({ path: file.path, text, url, sourceSha256: file.sha256 });
  }
  return { evidence, versionEvidence };
}

const cargoPackages = cargoMetadata.packages
  .filter((item) => item.source !== null)
  .sort((left, right) => compareCodePoints(cargoPurl(left.name, left.version), cargoPurl(right.name, right.version)));
for (const metadata of cargoPackages) {
  const packageId = `${metadata.name}@${metadata.version}`;
  const locked = cargoChecksums.get(packageId);
  if (!locked) throw new Error(`Cargo.lock checksum is missing for ${packageId}`);
  const terms = licenseTerms(metadata.license, knownLicenses, knownExceptions);
  const archive = await cargoArchive(packageId, metadata.name, metadata.version, locked.checksum);
  let evidence = archive.evidence.map((item) => ({
    ...item,
    kind: "cargo-archive",
    sourceSha256: locked.checksum,
  }));
  const remote = remoteByPackage.get(packageId);
  const canonicalOnly = canonicalOnlyByPackage.get(packageId);
  assertCanonicalOnlyNeeded(packageId, evidence, canonicalOnly);
  let remoteVersionEvidence = null;
  if (remote) {
    const extra = await remoteEvidence(packageId, metadata, archive, remote);
    remoteVersionEvidence = extra.versionEvidence;
    evidence = [...evidence, ...extra.evidence.map((item) => ({ ...item, kind: "cargo-upstream-revision" }))];
  }
  if (!evidence.length && !canonicalOnly) {
    throw new Error(`Cargo package has no license or notice evidence: ${packageId}`);
  }
  if (canonicalOnly) {
    await verifyCargoSource(packageId, metadata, archive, canonicalOnly, { requireRevision: true });
  }
  const evidenceTextIds = evidence.map((item) => registerText(item.text, item.kind === "cargo-archive"
    ? { kind: item.kind, package: packageId, crateSha256: item.sourceSha256, path: item.path }
    : {
      kind: item.kind,
      package: packageId,
      repository: remote.repository,
      revision: remote.revision,
      path: item.path,
      sourceSha256: item.sourceSha256,
      url: item.url,
    }));
  components.push({
    ecosystem: "cargo",
    name: metadata.name,
    version: metadata.version,
    purl: cargoPurl(metadata.name, metadata.version),
    scope: "runtime-or-build",
    licenseExpression: metadata.license,
    normalizedLicenseExpression: terms.normalized,
    canonicalTextIds: await canonicalTextIds(terms),
    evidenceTextIds: [...new Set(evidenceTextIds)].sort(compareCodePoints),
    ...(remoteVersionEvidence ? { upstreamVersionEvidence: remoteVersionEvidence } : {}),
    ...(canonicalOnly ? {
      reviewedCanonicalOnly: {
        repository: canonicalOnly.repository,
        revision: canonicalOnly.revision,
        reason: canonicalOnly.reason,
      },
    } : {}),
  });
}

components.sort((left, right) => compareCodePoints(left.purl, right.purl));
if (new Set(components.map((item) => item.purl)).size !== components.length) {
  throw new Error("Third-party license components are not uniquely identified");
}
assertNoStaleLicenseMappings(policy, {
  usedNpmFamilyPrefixes,
  cargoPackageIds: new Set(cargoPackages.map((item) => `${item.name}@${item.version}`)),
});

const texts = [...textById.values()]
  .map((item) => ({ ...item, sources: item.sources.sort((left, right) => compareCodePoints(JSON.stringify(left), JSON.stringify(right))) }))
  .sort((left, right) => compareCodePoints(left.id, right.id));
const manifest = {
  schemaVersion: 1,
  project: { name: packageJson.name, version: packageJson.version, licenseDecision: packageJson.license },
  selection: "complete npm package-lock graph and complete external Cargo resolve graph",
  inputs: {
    npm: "package-lock.json child and parent archives verified by SRI; platform binary packages may inherit evidence only from a verified exact parent declaration",
    cargo: "Cargo.lock crates.io archives verified by checksum; omitted texts use pinned upstream revisions with archive, tag, or manifest version evidence",
    canonical: policy.canonical.version,
  },
  componentCount: components.length,
  textCount: texts.length,
  components,
  texts: texts.map(({ text: _text, ...item }) => item),
};
const bundle = [
  "WakeGPT Third-Party License Texts",
  "",
  "This generated file contains every unique package notice and pinned canonical license text",
  "referenced by the adjacent manifest. WakeGPT itself is licensed under Apache-2.0; this file",
  "covers third-party components only.",
  "",
  ...texts.flatMap((item) => [
    `================================================================================`,
    `${item.id}`,
    `================================================================================`,
    item.text.replace(/\n$/u, ""),
    "",
  ]),
].join("\n");

const manifestPath = join(outputDirectory, `wakegpt-${packageJson.version}-third-party-licenses.json`);
const bundlePath = join(outputDirectory, "THIRD-PARTY-LICENSES.txt");
await installOutputDirectory(outputDirectory, [
  [basename(manifestPath), `${JSON.stringify(manifest, null, 2)}\n`],
  [basename(bundlePath), bundle],
]);
console.log(JSON.stringify({
  componentCount: components.length,
  npmComponentCount: npmEntries.length,
  cargoComponentCount: cargoPackages.length,
  textCount: texts.length,
  outputs: [manifestPath, bundlePath].map((path) => path.slice(dirname(outputDirectory).length + 1)),
}));
