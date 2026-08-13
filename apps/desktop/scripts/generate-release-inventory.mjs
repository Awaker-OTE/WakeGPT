import { spawnSync } from "node:child_process";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = fileURLToPath(new URL("..", import.meta.url));
const workspaceRoot = resolve(appRoot, "../..");
const cargoRoot = join(appRoot, "src-tauri");

const parseArguments = (arguments_) => {
  let outputDirectory = join(workspaceRoot, "release-output", "supply-chain");
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
};

const packageNameFromPath = (packagePath) => {
  const marker = "node_modules/";
  const markerIndex = packagePath.lastIndexOf(marker);
  if (markerIndex < 0) throw new Error(`Invalid package-lock path: ${packagePath}`);
  return packagePath.slice(markerIndex + marker.length);
};

const npmPurlName = (name) => {
  if (!name.startsWith("@")) return encodeURIComponent(name);
  const separator = name.indexOf("/");
  if (separator < 0) throw new Error(`Invalid scoped npm package: ${name}`);
  return `${encodeURIComponent(name.slice(0, separator))}/${encodeURIComponent(name.slice(separator + 1))}`;
};

const licenseEvidence = (license) => {
  if (!license) throw new Error("A resolved dependency is missing license metadata");
  if (/^[A-Za-z0-9.+-]+$/u.test(license)) return [{ license: { id: license } }];
  if (!license.includes("/")) return [{ expression: license }];
  return [{ license: { name: license } }];
};

const npmHash = (integrity) => {
  if (!integrity) return [];
  const separator = integrity.indexOf("-");
  if (separator < 1) return [];
  const algorithm = integrity.slice(0, separator).toLowerCase();
  const algorithmNames = { sha256: "SHA-256", sha384: "SHA-384", sha512: "SHA-512" };
  const name = algorithmNames[algorithm];
  if (!name) return [];
  return [{ alg: name, content: Buffer.from(integrity.slice(separator + 1), "base64").toString("hex") }];
};

const resolveNpmDependencyPath = (packagePaths, fromPath, dependencyName) => {
  let current = fromPath;
  while (true) {
    const candidate = current
      ? `${current}/node_modules/${dependencyName}`
      : `node_modules/${dependencyName}`;
    if (packagePaths.has(candidate)) return candidate;
    if (!current) return null;
    const nestedMarker = current.lastIndexOf("/node_modules/");
    current = nestedMarker >= 0 ? current.slice(0, nestedMarker) : "";
  }
};

const uniqueSorted = (values) => [...new Set(values)].sort();

const { outputDirectory } = parseArguments(process.argv.slice(2));
const [packageJson, packageLock] = await Promise.all([
  readFile(join(appRoot, "package.json"), "utf8").then(JSON.parse),
  readFile(join(appRoot, "package-lock.json"), "utf8").then(JSON.parse),
]);
if (packageJson.license !== "Apache-2.0" || packageLock.packages?.[""]?.license !== packageJson.license) {
  throw new Error("WakeGPT project license metadata must consistently declare Apache-2.0");
}

const cargoResult = spawnSync(
  "cargo",
  ["metadata", "--locked", "--offline", "--format-version", "1"],
  { cwd: cargoRoot, encoding: "utf8", maxBuffer: 128 * 1024 * 1024 },
);
if (cargoResult.error) throw cargoResult.error;
if (cargoResult.status !== 0) {
  throw new Error(cargoResult.stderr.trim() || "cargo metadata failed");
}
const cargoMetadata = JSON.parse(cargoResult.stdout);

const npmEntries = Object.entries(packageLock.packages)
  .filter(([packagePath]) => packagePath.startsWith("node_modules/"))
  .sort(([first], [second]) => first.localeCompare(second));
const npmPackagePaths = new Set(npmEntries.map(([packagePath]) => packagePath));
const npmRefByPath = new Map();
const components = [];
const licenseComponents = [];

for (const [packagePath, metadata] of npmEntries) {
  const name = metadata.name || packageNameFromPath(packagePath);
  const version = metadata.version;
  if (!version) throw new Error(`${packagePath} is missing a resolved version`);
  const purl = `pkg:npm/${npmPurlName(name)}@${encodeURIComponent(version)}`;
  if ([...npmRefByPath.values()].includes(purl)) {
    throw new Error(`Duplicate npm package identity requires a qualified purl: ${name}@${version}`);
  }
  npmRefByPath.set(packagePath, purl);
  const scope = metadata.dev === true ? "excluded" : "required";
  components.push({
    type: "library",
    name,
    version,
    "bom-ref": purl,
    purl,
    scope,
    licenses: licenseEvidence(metadata.license),
    ...(npmHash(metadata.integrity).length ? { hashes: npmHash(metadata.integrity) } : {}),
    ...(metadata.resolved
      ? { externalReferences: [{ type: "distribution", url: metadata.resolved }] }
      : {}),
    properties: [
      { name: "wakegpt:ecosystem", value: "npm" },
      { name: "wakegpt:package-lock-path", value: packagePath },
      { name: "wakegpt:dependency-role", value: scope === "required" ? "production" : "development" },
    ],
  });
  licenseComponents.push({
    ecosystem: "npm",
    name,
    version,
    license: metadata.license,
    purl,
    scope,
    source: metadata.resolved || null,
  });
}

const cargoRefById = new Map();
for (const metadata of cargoMetadata.packages.filter((item) => item.source !== null)) {
  const purl = `pkg:cargo/${encodeURIComponent(metadata.name)}@${encodeURIComponent(metadata.version)}`;
  if ([...cargoRefById.values()].includes(purl)) {
    throw new Error(`Duplicate Cargo package identity requires a qualified purl: ${metadata.name}@${metadata.version}`);
  }
  cargoRefById.set(metadata.id, purl);
  components.push({
    type: "library",
    name: metadata.name,
    version: metadata.version,
    "bom-ref": purl,
    purl,
    scope: "required",
    licenses: licenseEvidence(metadata.license),
    ...(metadata.repository
      ? { externalReferences: [{ type: "vcs", url: metadata.repository }] }
      : {}),
    properties: [
      { name: "wakegpt:ecosystem", value: "cargo" },
      { name: "wakegpt:cargo-source", value: metadata.source },
    ],
  });
  licenseComponents.push({
    ecosystem: "cargo",
    name: metadata.name,
    version: metadata.version,
    license: metadata.license,
    purl,
    scope: "required",
    source: metadata.repository || metadata.source,
  });
}

components.sort((first, second) => first["bom-ref"].localeCompare(second["bom-ref"]));
licenseComponents.sort((first, second) => first.purl.localeCompare(second.purl));

const dependencyMap = new Map(components.map((component) => [component["bom-ref"], []]));
for (const [packagePath, metadata] of npmEntries) {
  const reference = npmRefByPath.get(packagePath);
  const requiredNames = Object.keys(metadata.dependencies || {});
  const optionalNames = Object.keys(metadata.optionalDependencies || {});
  const peerNames = Object.keys(metadata.peerDependencies || {});
  const dependencies = [];
  for (const dependencyName of uniqueSorted([...requiredNames, ...optionalNames, ...peerNames])) {
    const resolvedPath = resolveNpmDependencyPath(npmPackagePaths, packagePath, dependencyName);
    if (!resolvedPath) {
      if (requiredNames.includes(dependencyName)) {
        throw new Error(`${packagePath} cannot resolve required dependency ${dependencyName}`);
      }
      continue;
    }
    dependencies.push(npmRefByPath.get(resolvedPath));
  }
  dependencyMap.set(reference, uniqueSorted(dependencies));
}

for (const node of cargoMetadata.resolve.nodes) {
  const reference = cargoRefById.get(node.id);
  if (!reference) continue;
  dependencyMap.set(
    reference,
    uniqueSorted(node.deps.map((dependency) => cargoRefById.get(dependency.pkg)).filter(Boolean)),
  );
}

const rootReference = `pkg:generic/wakegpt-desktop@${encodeURIComponent(packageJson.version)}`;
const rootDependencies = [];
for (const dependencyName of uniqueSorted([
  ...Object.keys(packageLock.packages[""].dependencies || {}),
  ...Object.keys(packageLock.packages[""].devDependencies || {}),
])) {
  const resolvedPath = resolveNpmDependencyPath(npmPackagePaths, "", dependencyName);
  if (!resolvedPath) throw new Error(`Root package cannot resolve ${dependencyName}`);
  rootDependencies.push(npmRefByPath.get(resolvedPath));
}
const cargoRootNode = cargoMetadata.resolve.nodes.find((node) => node.id === cargoMetadata.resolve.root);
if (!cargoRootNode) throw new Error("Cargo root node is missing");
rootDependencies.push(
  ...cargoRootNode.deps.map((dependency) => cargoRefById.get(dependency.pkg)).filter(Boolean),
);

const dependencies = [
  { ref: rootReference, dependsOn: uniqueSorted(rootDependencies) },
  ...[...dependencyMap.entries()]
    .sort(([first], [second]) => first.localeCompare(second))
    .map(([ref, dependsOn]) => ({ ref, dependsOn })),
];

const sbom = {
  bomFormat: "CycloneDX",
  specVersion: "1.6",
  version: 1,
  metadata: {
    tools: {
      components: [{ type: "application", name: "WakeGPT release inventory generator", version: "1" }],
    },
    component: {
      type: "application",
      name: "wakegpt-desktop",
      version: packageJson.version,
      "bom-ref": rootReference,
      purl: rootReference,
      licenses: [{ license: { id: packageJson.license } }],
    },
    properties: [
      { name: "wakegpt:inventory:inputs", value: "package-lock.json + cargo metadata --locked --offline" },
      { name: "wakegpt:inventory:timestamp-policy", value: "omitted-for-byte-reproducibility" },
    ],
  },
  components,
  dependencies,
};

const licenseInventory = {
  schemaVersion: 1,
  project: { name: "wakegpt-desktop", version: packageJson.version, licenseDecision: packageJson.license },
  selection: "complete npm package-lock graph and complete external Cargo resolve graph",
  componentCount: licenseComponents.length,
  components: licenseComponents,
};

await mkdir(outputDirectory, { recursive: true });
const sbomPath = join(outputDirectory, `wakegpt-${packageJson.version}.cdx.json`);
const licensePath = join(outputDirectory, `wakegpt-${packageJson.version}-licenses.json`);
await Promise.all([
  writeFile(sbomPath, `${JSON.stringify(sbom, null, 2)}\n`, "utf8"),
  writeFile(licensePath, `${JSON.stringify(licenseInventory, null, 2)}\n`, "utf8"),
]);

console.log(JSON.stringify({
  componentCount: components.length,
  npmComponentCount: npmEntries.length,
  cargoComponentCount: cargoRefById.size,
  outputs: [sbomPath, licensePath].map((path) => path.slice(dirname(outputDirectory).length + 1)),
}));
