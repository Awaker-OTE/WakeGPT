import { createHash, timingSafeEqual } from "node:crypto";
import { constants } from "node:fs";
import { lstat, mkdir, mkdtemp, open, rename, rm } from "node:fs/promises";
import { basename, dirname, join } from "node:path";
import { gunzipSync } from "node:zlib";

const TAR_BLOCK_BYTES = 512;
const DEFAULT_MAX_ARCHIVE_BYTES = 128 * 1024 * 1024;
const DEFAULT_MAX_UNPACKED_BYTES = 512 * 1024 * 1024;
const DEFAULT_MAX_TEXT_BYTES = 4 * 1024 * 1024;
const utf8 = new TextDecoder("utf-8", { fatal: true });

export function compareCodePoints(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

export function assertExactPackageIdentity(metadata, expected, label) {
  if (
    metadata?.name !== expected.name
    || metadata?.version !== expected.version
    || metadata?.license !== expected.license
  ) {
    throw new Error(`${label} identity or license differs from the lock graph`);
  }
}

export function verifyNpmFamilyRelationship({ name, version, license, parentName, child, parent }) {
  assertExactPackageIdentity(child.packageMetadata, { name, version, license }, `npm archive ${name}`);
  assertExactPackageIdentity(
    parent.packageMetadata,
    { name: parentName, version, license },
    `npm archive ${parentName}`,
  );
  if (parent.packageMetadata.optionalDependencies?.[name] !== version) {
    throw new Error(`verified npm family parent does not pin ${name}@${version}`);
  }
  return child.evidence.length ? { evidence: child.evidence, evidenceOwner: "child" }
    : { evidence: parent.evidence, evidenceOwner: "parent" };
}

function cargoPackageSection(source) {
  const text = normalizedText(source);
  const packageHeader = text.search(/^\[package\]\s*$/mu);
  if (packageHeader < 0) throw new Error("Pinned Cargo manifest is missing a package section");
  const afterHeader = text.slice(packageHeader).replace(/^\[package\]\s*\n?/u, "");
  const nextSection = afterHeader.search(/^\[/mu);
  const section = nextSection < 0 ? afterHeader : afterHeader.slice(0, nextSection);
  const read = (key) => section.match(new RegExp(`^${key}\\s*=\\s*"([^"]+)"\\s*$`, "mu"))?.[1] ?? null;
  return { name: read("name"), version: read("version"), license: read("license") };
}

export async function verifyCargoSourceBinding({
  packageId,
  metadata,
  archiveRevision,
  entry,
  requireRevision = false,
  fetchManifest,
  resolveGitTag,
}) {
  const repository = String(metadata.repository ?? "")
    .replace(/^git\+/u, "")
    .replace(/\.git\/?$/u, "")
    .replace(/\/+$/u, "");
  if (repository && repository !== entry.repository) {
    throw new Error(`Cargo repository mismatch: ${packageId}`);
  }
  if (archiveRevision && archiveRevision !== entry.revision) {
    throw new Error(`Cargo source revision mismatch: ${packageId}`);
  }
  if (requireRevision && !archiveRevision) {
    throw new Error(`Cargo archive lacks required VCS revision: ${packageId}`);
  }
  if (!repository && !archiveRevision) {
    throw new Error(`Cargo source cannot be tied to reviewed evidence: ${packageId}`);
  }

  const versionEvidence = entry.versionEvidence ?? { kind: "archive-vcs" };
  if (versionEvidence.kind === "archive-vcs") {
    if (!archiveRevision) throw new Error(`Cargo archive lacks VCS version evidence: ${packageId}`);
    return { kind: "archive-vcs", revision: archiveRevision };
  }
  if (archiveRevision) {
    throw new Error(`Cargo mapping has redundant external version evidence: ${packageId}`);
  }
  if (versionEvidence.version !== metadata.version) {
    throw new Error(`Cargo version evidence does not match ${packageId}`);
  }
  if (versionEvidence.kind === "git-tag") {
    const resolved = await resolveGitTag(entry.repository, versionEvidence.tag);
    if (
      resolved.tagObject !== versionEvidence.tagObject
      || resolved.revision !== entry.revision
    ) {
      throw new Error(`Cargo Git tag does not resolve to the reviewed revision: ${packageId}`);
    }
    return { kind: "git-tag", tag: versionEvidence.tag, tagObject: resolved.tagObject };
  }
  if (versionEvidence.kind === "manifest") {
    const bytes = await fetchManifest(entry, versionEvidence);
    verifyHexDigest(bytes, "sha256", versionEvidence.sha256);
    const identity = cargoPackageSection(bytes);
    assertExactPackageIdentity(identity, metadata, `Cargo upstream manifest ${packageId}`);
    return {
      kind: "manifest",
      path: versionEvidence.path,
      sha256: versionEvidence.sha256,
    };
  }
  throw new Error(`Unsupported Cargo version evidence for ${packageId}`);
}

export function assertNoStaleLicenseMappings(
  policy,
  { usedNpmFamilyPrefixes, cargoPackageIds },
) {
  for (const family of policy.npmFamilies) {
    if (!usedNpmFamilyPrefixes.has(family.prefix)) {
      throw new Error(`Stale npm family license mapping: ${family.prefix}`);
    }
  }
  for (const entry of policy.cargoRemoteEvidence) {
    for (const packageId of entry.packages) {
      if (!cargoPackageIds.has(packageId)) {
        throw new Error(`Stale Cargo evidence mapping: ${packageId}`);
      }
    }
  }
  for (const entry of policy.cargoCanonicalOnly) {
    if (!cargoPackageIds.has(entry.package)) {
      throw new Error(`Stale canonical-only mapping: ${entry.package}`);
    }
  }
}

export function assertCanonicalOnlyNeeded(packageId, evidence, mapping) {
  if (mapping && evidence.length > 0) {
    throw new Error(`Stale canonical-only mapping now has archive evidence: ${packageId}`);
  }
}

function decodeField(buffer) {
  const end = buffer.indexOf(0);
  return utf8.decode(end >= 0 ? buffer.subarray(0, end) : buffer).trim();
}

function parseOctal(buffer, label) {
  const value = decodeField(buffer).replace(/\s+$/u, "");
  if (!/^[0-7]*$/u.test(value)) throw new Error(`Invalid tar ${label}`);
  const parsed = value ? Number.parseInt(value, 8) : 0;
  if (!Number.isSafeInteger(parsed) || parsed < 0) throw new Error(`Invalid tar ${label}`);
  return parsed;
}

function validateTarChecksum(header) {
  const expected = parseOctal(header.subarray(148, 156), "checksum");
  let actual = 0;
  for (let index = 0; index < header.length; index += 1) {
    actual += index >= 148 && index < 156 ? 0x20 : header[index];
  }
  if (actual !== expected) throw new Error("Tar header checksum mismatch");
}

function safeArchivePath(value) {
  const normalized = value.replace(/\\/gu, "/").replace(/^\.\//u, "").replace(/\/+$/u, "");
  if (
    !normalized
    || normalized.startsWith("/")
    || normalized.includes("\0")
    || normalized.split("/").some((part) => part === "" || part === "." || part === "..")
  ) {
    throw new Error("Archive contains an unsafe path");
  }
  return normalized;
}

function parsePax(buffer) {
  const values = {};
  let offset = 0;
  while (offset < buffer.length) {
    const space = buffer.indexOf(0x20, offset);
    if (space < 0) throw new Error("Invalid PAX record length");
    const lengthText = buffer.subarray(offset, space).toString("ascii");
    if (!/^[1-9][0-9]*$/u.test(lengthText)) throw new Error("Invalid PAX record length");
    const length = Number.parseInt(lengthText, 10);
    const end = offset + length;
    if (!Number.isSafeInteger(length) || end > buffer.length || buffer[end - 1] !== 0x0a) {
      throw new Error("Invalid PAX record boundary");
    }
    const record = utf8.decode(buffer.subarray(space + 1, end - 1));
    const separator = record.indexOf("=");
    if (separator < 1) throw new Error("Invalid PAX record");
    values[record.slice(0, separator)] = record.slice(separator + 1);
    offset = end;
  }
  return values;
}

export function parseTarGzip(
  archive,
  {
    maxArchiveBytes = DEFAULT_MAX_ARCHIVE_BYTES,
    maxUnpackedBytes = DEFAULT_MAX_UNPACKED_BYTES,
  } = {},
) {
  if (!Buffer.isBuffer(archive) || archive.length === 0 || archive.length > maxArchiveBytes) {
    throw new Error("Archive size is outside the allowed range");
  }
  let unpacked;
  try {
    unpacked = gunzipSync(archive, { maxOutputLength: maxUnpackedBytes });
  } catch {
    throw new Error("Archive is not a bounded valid gzip stream");
  }

  const entries = [];
  let offset = 0;
  let nextPath = null;
  let nextPax = null;
  let globalPax = {};
  while (offset + TAR_BLOCK_BYTES <= unpacked.length) {
    const header = unpacked.subarray(offset, offset + TAR_BLOCK_BYTES);
    if (header.every((byte) => byte === 0)) break;
    validateTarChecksum(header);

    const name = decodeField(header.subarray(0, 100));
    const prefix = decodeField(header.subarray(345, 500));
    const headerPath = prefix ? `${prefix}/${name}` : name;
    const size = parseOctal(header.subarray(124, 136), "size");
    const dataStart = offset + TAR_BLOCK_BYTES;
    const dataEnd = dataStart + size;
    if (dataEnd > unpacked.length) throw new Error("Tar entry extends beyond the archive");
    const data = unpacked.subarray(dataStart, dataEnd);
    const type = String.fromCharCode(header[156] || 0x30);

    if (type === "x" || type === "g") {
      const parsed = parsePax(data);
      if (type === "g") globalPax = { ...globalPax, ...parsed };
      else nextPax = parsed;
    } else if (type === "L") {
      nextPath = utf8.decode(data).replace(/[\0\n]+$/gu, "");
    } else {
      const pax = { ...globalPax, ...(nextPax ?? {}) };
      const path = safeArchivePath(pax.path ?? nextPath ?? headerPath);
      if (type === "0" || type === "\0") entries.push({ path, type: "file", data });
      else if (type === "5") entries.push({ path, type: "directory", data: Buffer.alloc(0) });
      nextPath = null;
      nextPax = null;
    }

    offset = dataStart + Math.ceil(size / TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES;
  }
  if (!entries.length) throw new Error("Archive does not contain any supported entries");
  return entries;
}

const LICENSE_EVIDENCE_BASENAMES = new Set([
  "authors",
  "copying",
  "copying-cmake-scripts",
  "copyright",
  "copyright-rust.txt",
  "copyright.md",
  "licence",
  "license",
  "license-0bsd",
  "license-apache",
  "license-apache-2.0",
  "license-apache-2.0_with_llvm-exception",
  "license-apache.md",
  "license-boringSSL",
  "license-bsd",
  "license-isc",
  "license-libm-mit",
  "license-mit",
  "license-mit.md",
  "license-mit.txt",
  "license-other-bits",
  "license-third-party",
  "license-unicode",
  "license-zlib",
  "license-zlib.md",
  "license.apache-2.0",
  "license.bsd-3-clause",
  "license.mit",
  "license.md",
  "license.spdx",
  "license.txt",
  "license_apache-2.0",
  "license_mit",
  "licenses",
  "notice",
  "notice.md",
  "notice.rst",
  "notice.txt",
  "unlicense",
].map((name) => name.toLowerCase()));

export function isLicenseEvidencePath(path) {
  const basename = path.split("/").at(-1) ?? "";
  return LICENSE_EVIDENCE_BASENAMES.has(basename.toLowerCase());
}

export function normalizedText(data, maxBytes = DEFAULT_MAX_TEXT_BYTES) {
  if (!Buffer.isBuffer(data) || data.length === 0 || data.length > maxBytes) {
    throw new Error("License text size is outside the allowed range");
  }
  let text;
  try {
    text = utf8.decode(data);
  } catch {
    throw new Error("License evidence must be valid UTF-8 text");
  }
  if (text.includes("\0")) throw new Error("License evidence contains a NUL byte");
  return `${text.replace(/\r\n?/gu, "\n").replace(/\n*$/u, "")}\n`;
}

export function archiveEvidence(entries, rootPrefix) {
  const prefix = safeArchivePath(rootPrefix);
  const root = `${prefix}/`;
  const files = new Map();
  for (const entry of entries) {
    if (entry.path !== prefix && !entry.path.startsWith(root)) {
      throw new Error(`Archive entry escapes the expected package root: ${entry.path}`);
    }
    if (entry.type !== "file") continue;
    const relativePath = entry.path.slice(root.length);
    if (!relativePath || files.has(relativePath)) {
      throw new Error("Archive contains an empty or duplicate file path");
    }
    files.set(relativePath, entry.data);
  }
  const evidence = [...files.entries()]
    .filter(([path]) => isLicenseEvidencePath(path))
    .map(([path, data]) => ({ path, text: normalizedText(data) }))
    .sort((left, right) => compareCodePoints(left.path, right.path));
  return { files, evidence };
}

export function archiveSingleRoot(entries, requiredRelativePath) {
  if (!Array.isArray(entries) || entries.length === 0) {
    throw new Error("Archive does not contain any entries");
  }
  const roots = new Set(entries.map((entry) => entry.path.split("/")[0]));
  if (roots.size !== 1) throw new Error("Archive must have exactly one top-level directory");
  const root = [...roots][0];
  const requiredPath = `${root}/${safeArchivePath(requiredRelativePath)}`;
  if (!entries.some((entry) => entry.type === "file" && entry.path === requiredPath)) {
    throw new Error(`Archive is missing ${requiredRelativePath} at its package root`);
  }
  return root;
}

export function sha256(data) {
  return createHash("sha256").update(data).digest("hex");
}

export function selectVerifiedCargoArchive(packageId, checksum, candidates) {
  if (!Array.isArray(candidates) || candidates.length === 0) {
    throw new Error(`Verified Cargo archive unavailable for ${packageId}; run cargo fetch --locked`);
  }
  const match = candidates.find((bytes) => Buffer.isBuffer(bytes) && sha256(bytes) === checksum);
  if (!match) throw new Error(`Cached Cargo archive checksum mismatch for ${packageId}`);
  return match;
}

export function verifyHexDigest(data, algorithm, expected) {
  if (!/^[0-9a-f]+$/u.test(expected) || expected.length % 2 !== 0) {
    throw new Error("Expected digest must be lowercase hexadecimal");
  }
  const actual = createHash(algorithm).update(data).digest();
  const expectedBytes = Buffer.from(expected, "hex");
  if (actual.length !== expectedBytes.length || !timingSafeEqual(actual, expectedBytes)) {
    throw new Error("Archive checksum mismatch");
  }
}

export function verifySri(data, integrity) {
  const candidates = String(integrity).trim().split(/\s+/u).map((item) => {
    const separator = item.indexOf("-");
    if (separator < 1) return null;
    return { algorithm: item.slice(0, separator), digest: item.slice(separator + 1) };
  }).filter(Boolean);
  const priority = ["sha512", "sha384", "sha256"];
  const candidate = priority
    .map((algorithm) => candidates.find((item) => item.algorithm === algorithm))
    .find(Boolean);
  if (!candidate || !/^[A-Za-z0-9+/]+={0,2}$/u.test(candidate.digest)) {
    throw new Error("Package lock entry lacks a supported SRI digest");
  }
  const actual = createHash(candidate.algorithm).update(data).digest();
  const expected = Buffer.from(candidate.digest, "base64");
  if (actual.length !== expected.length || !timingSafeEqual(actual, expected)) {
    throw new Error("Package archive SRI mismatch");
  }
  return { algorithm: candidate.algorithm, hex: actual.toString("hex") };
}

export async function fetchBoundedHttps(
  url,
  {
    allowedHosts,
    expectedSha256 = null,
    fetchImpl = globalThis.fetch,
    maxBytes = DEFAULT_MAX_TEXT_BYTES,
    timeoutMs = 60_000,
  } = {},
) {
  const parsed = new URL(url);
  const hosts = new Set(allowedHosts ?? []);
  if (
    parsed.protocol !== "https:"
    || parsed.username
    || parsed.password
    || !hosts.has(parsed.hostname)
  ) {
    throw new Error("License source must use an explicitly allowed credential-free HTTPS host");
  }
  if (!Number.isSafeInteger(maxBytes) || maxBytes <= 0) {
    throw new Error("License source size limit must be a positive safe integer");
  }
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 60_000) {
    throw new Error("License source timeout must be between 1 and 60000 milliseconds");
  }

  let response;
  try {
    response = await fetchImpl(parsed, {
      redirect: "error",
      signal: AbortSignal.timeout(timeoutMs),
      headers: { "User-Agent": "WakeGPT-license-bundle/1.0" },
    });
  } catch (error) {
    if (error?.name === "AbortError" || error?.name === "TimeoutError") {
      throw new Error(`License source request timed out after ${timeoutMs} milliseconds`);
    }
    throw new Error(`License source request failed for ${parsed.hostname}`);
  }
  if (!response.ok || !response.body) {
    throw new Error(`License source request failed with HTTP ${response.status}`);
  }
  const contentLengthHeader = response.headers.get("content-length");
  if (contentLengthHeader !== null && !/^(?:0|[1-9][0-9]*)$/u.test(contentLengthHeader)) {
    throw new Error("License source returned an invalid content length");
  }
  const contentLength = Number(contentLengthHeader ?? 0);
  if (!Number.isSafeInteger(contentLength) || contentLength > maxBytes) {
    throw new Error("License source exceeds the allowed size");
  }

  const chunks = [];
  let length = 0;
  for await (const chunk of response.body) {
    const bytes = Buffer.from(chunk);
    length += bytes.length;
    if (length > maxBytes) throw new Error("License source exceeds the allowed size");
    chunks.push(bytes);
  }
  const bytes = Buffer.concat(chunks);
  if (expectedSha256) verifyHexDigest(bytes, "sha256", expectedSha256);
  return bytes;
}

export async function installOutputDirectory(outputDirectory, outputs) {
  if (!Array.isArray(outputs) || outputs.length === 0) {
    throw new Error("At least one generated output is required");
  }
  const names = outputs.map(([name]) => name);
  if (names.some((name) => !name || basename(name) !== name) || new Set(names).size !== names.length) {
    throw new Error("Generated output names must be unique basenames");
  }

  const parent = dirname(outputDirectory);
  const outputName = basename(outputDirectory);
  const lockDirectory = `${outputDirectory}.lock`;
  await mkdir(parent, { recursive: true });
  try {
    await mkdir(lockDirectory, { mode: 0o700 });
  } catch (error) {
    if (error?.code === "EEXIST") {
      throw new Error(`Output generation is already locked: ${outputName}`);
    }
    throw error;
  }
  let staging = null;
  try {
    try {
      await lstat(outputDirectory);
      throw new Error(`Output directory already exists: ${outputName}`);
    } catch (error) {
      if (error?.code !== "ENOENT") throw error;
    }
    staging = await mkdtemp(join(parent, `.${outputName}.partial-`));
    for (const [name, contents] of outputs) {
      const handle = await open(
        join(staging, name),
        constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY,
        0o644,
      );
      try {
        await handle.writeFile(contents, "utf8");
        await handle.sync();
      } finally {
        await handle.close();
      }
    }
    const stagingHandle = await open(staging, constants.O_RDONLY);
    try {
      await stagingHandle.sync();
    } finally {
      await stagingHandle.close();
    }
    await rename(staging, outputDirectory);
    staging = null;
    const parentHandle = await open(parent, constants.O_RDONLY);
    try {
      await parentHandle.sync();
    } finally {
      await parentHandle.close();
    }
  } catch (error) {
    if (error?.code === "EEXIST" || error?.code === "ENOTEMPTY") {
      throw new Error(`Output directory already exists: ${outputName}`);
    }
    throw error;
  } finally {
    if (staging) await rm(staging, { recursive: true, force: true });
    await rm(lockDirectory, { recursive: true, force: true });
  }
}

export function licenseTerms(expression, knownLicenseIds, knownExceptionIds) {
  if (typeof expression !== "string" || !expression.trim()) {
    throw new Error("Dependency is missing a license expression");
  }
  const normalized = expression.replace(/\s*\/\s*/gu, " OR ").replace(/\s+/gu, " ").trim();
  if (!/^[A-Za-z0-9.()+\- ]+$/u.test(normalized)) {
    throw new Error(`Unsupported license expression syntax: ${expression}`);
  }
  const tokens = normalized.match(/\(|\)|[A-Za-z0-9][A-Za-z0-9.+-]*/gu) ?? [];
  if (tokens.join(" ").replace(/\( /gu, "(").replace(/ \)/gu, ")") !== normalized) {
    throw new Error(`Unsupported license expression syntax: ${expression}`);
  }
  const licenses = [];
  const exceptions = [];
  let index = 0;
  const peek = () => tokens[index];
  const take = () => tokens[index++];
  const parsePrimary = () => {
    if (peek() === "(") {
      take();
      parseOr();
      if (take() !== ")") throw new Error(`Unbalanced license expression: ${expression}`);
      return;
    }
    const token = take();
    if (!token || !knownLicenseIds.has(token)) {
      throw new Error(`No pinned canonical text exists for license term: ${token ?? "<missing>"}`);
    }
    licenses.push(token);
  };
  const parseWith = () => {
    parsePrimary();
    if (peek() !== "WITH") return;
    take();
    const exception = take();
    if (!exception || !knownExceptionIds.has(exception)) {
      throw new Error(`No pinned canonical text exists for license exception: ${exception ?? "<missing>"}`);
    }
    exceptions.push(exception);
  };
  const parseAnd = () => {
    parseWith();
    while (peek() === "AND") {
      take();
      parseWith();
    }
  };
  function parseOr() {
    parseAnd();
    while (peek() === "OR") {
      take();
      parseAnd();
    }
  }
  parseOr();
  if (index !== tokens.length) throw new Error(`Invalid license expression: ${expression}`);
  return {
    normalized,
    licenses: [...new Set(licenses)].sort(compareCodePoints),
    exceptions: [...new Set(exceptions)].sort(compareCodePoints),
  };
}
