#!/usr/bin/env node
// Release helpers for .github/workflows/release.yml.
//
//   node scripts/ci/release.mjs info [--tag <tag>] [--metadata <file>]
//     Prints `version=<x.y.z>` and `desktop_bin=<name>` lines for $GITHUB_OUTPUT.
//     The version comes from `cargo metadata`: bucketbridge-cli and
//     bucketbridge-desktop must have the same version, and so must
//     crates/desktop/tauri.conf.json when it sets one. With --tag (a tag name
//     or refs/tags/... ref) the tag must be v<version>. The version must be
//     MAJOR.MINOR.PATCH within the limits of an MSI ProductVersion.
//     desktop_bin is the file name, without .exe, of the desktop executable
//     that `tauri build` leaves in target/<profile>/: `mainBinaryName` from
//     tauri.conf.json if set, otherwise the package's Cargo binary.
//     --metadata reads `cargo metadata --no-deps --format-version 1` output
//     from a file instead of running cargo.
//
//   node scripts/ci/release.mjs pick <dir> <suffix> <destination>
//     Copies the single regular file in <dir> whose name ends with <suffix>
//     to <destination>. Fails when there is no match or more than one, and
//     when <destination> already exists.

import { spawnSync } from 'node:child_process';
import { constants, readFileSync } from 'node:fs';
import { copyFile, mkdir, readdir } from 'node:fs/promises';
import path from 'node:path';

const CLI_PACKAGE = 'bucketbridge-cli';
const DESKTOP_PACKAGE = 'bucketbridge-desktop';

class UsageError extends Error {}

function readJson(file, what) {
  let text;
  try {
    text = readFileSync(file, 'utf8');
  } catch (error) {
    throw new Error(`cannot read ${what} ${file}: ${error.message}`);
  }
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new Error(`${what} ${file} is not valid JSON: ${error.message}`);
  }
}

function cargoMetadata() {
  const result = spawnSync('cargo', ['metadata', '--no-deps', '--format-version', '1', '--locked'], {
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error) {
    throw new Error(`cannot run cargo metadata: ${result.error.message}`);
  }
  if (result.status !== 0) {
    throw new Error(`cargo metadata failed:\n${result.stderr}`);
  }
  return JSON.parse(result.stdout);
}

function findPackage(metadata, name) {
  const found = metadata.packages.filter((p) => p.name === name);
  if (found.length !== 1) {
    throw new Error(`expected one workspace package named ${name}, found ${found.length}`);
  }
  return found[0];
}

function checkVersion(version) {
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(version);
  if (!match) {
    throw new Error(`version ${version} must be MAJOR.MINOR.PATCH with no pre-release or build suffix (MSI requirement)`);
  }
  const [major, minor, patch] = match.slice(1).map(Number);
  if (major > 255 || minor > 255 || patch > 65535) {
    throw new Error(`version ${version} exceeds the MSI limits 255.255.65535`);
  }
}

function tauriConfigVersion(configPath, config) {
  if (config.version === undefined || config.version === null) return null;
  if (typeof config.version !== 'string') {
    throw new Error(`${configPath}: "version" must be a string`);
  }
  if (!config.version.endsWith('.json')) return config.version;
  // Tauri also accepts a path, relative to tauri.conf.json, to a package.json-style file.
  const versionFile = path.resolve(path.dirname(configPath), config.version);
  const versionJson = readJson(versionFile, 'version file');
  if (typeof versionJson.version !== 'string') {
    throw new Error(`${versionFile} has no string "version" field`);
  }
  return versionJson.version;
}

function desktopBinary(desktop, config, configPath) {
  if (config.mainBinaryName !== undefined && config.mainBinaryName !== null) {
    if (typeof config.mainBinaryName !== 'string' || config.mainBinaryName === '') {
      throw new Error(`${configPath}: "mainBinaryName" must be a non-empty string`);
    }
    return config.mainBinaryName;
  }
  const bins = desktop.targets.filter((t) => t.kind.includes('bin')).map((t) => t.name);
  if (desktop.default_run) {
    if (!bins.includes(desktop.default_run)) {
      throw new Error(`${DESKTOP_PACKAGE}: default-run ${desktop.default_run} is not one of its binaries`);
    }
    return desktop.default_run;
  }
  if (bins.length === 1) return bins[0];
  if (bins.includes(desktop.name)) return desktop.name;
  throw new Error(`${DESKTOP_PACKAGE}: cannot tell its main binary from [${bins.join(', ')}]; set default-run or mainBinaryName`);
}

function info(args) {
  let tag = null;
  let metadataFile = null;
  for (let i = 0; i < args.length; i += 2) {
    const value = args[i + 1];
    if (value === undefined) throw new UsageError(`missing value for ${args[i]}`);
    if (args[i] === '--tag') tag = value;
    else if (args[i] === '--metadata') metadataFile = value;
    else throw new UsageError(`unknown option ${args[i]}`);
  }

  const metadata = metadataFile ? readJson(metadataFile, 'cargo metadata') : cargoMetadata();
  const cli = findPackage(metadata, CLI_PACKAGE);
  const desktop = findPackage(metadata, DESKTOP_PACKAGE);
  const version = cli.version;
  if (desktop.version !== version) {
    throw new Error(`${CLI_PACKAGE} is ${version} but ${DESKTOP_PACKAGE} is ${desktop.version}`);
  }

  const configPath = path.join(path.dirname(desktop.manifest_path), 'tauri.conf.json');
  const config = readJson(configPath, 'Tauri config');
  const configVersion = tauriConfigVersion(configPath, config);
  if (configVersion !== null && configVersion !== version) {
    throw new Error(`${configPath} sets version ${configVersion} but the crates are ${version}`);
  }
  checkVersion(version);

  if (tag !== null) {
    const tagName = tag.replace(/^refs\/tags\//, '');
    if (tagName !== `v${version}`) {
      throw new Error(`tag ${tagName} does not match the workspace version ${version} (expected v${version})`);
    }
  }

  const bin = desktopBinary(desktop, config, configPath);
  process.stdout.write(`version=${version}\ndesktop_bin=${bin}\n`);
}

async function pick(args) {
  if (args.length !== 3) {
    throw new UsageError('usage: node scripts/ci/release.mjs pick <dir> <suffix> <destination>');
  }
  const [dir, suffix, destination] = args;
  const entries = await readdir(dir, { withFileTypes: true });
  const matches = entries.filter((e) => e.isFile() && e.name.endsWith(suffix)).map((e) => e.name);
  if (matches.length !== 1) {
    throw new Error(`expected exactly one file ending in ${suffix} in ${dir}, found ${matches.length}${matches.length ? `: ${matches.join(', ')}` : ''}`);
  }
  const source = path.join(dir, matches[0]);
  await mkdir(path.dirname(path.resolve(destination)), { recursive: true });
  await copyFile(source, destination, constants.COPYFILE_EXCL);
  console.log(`${source} -> ${destination}`);
}

async function main(argv) {
  const [command, ...args] = argv;
  if (command === 'info') return info(args);
  if (command === 'pick') return pick(args);
  throw new UsageError('usage: node scripts/ci/release.mjs info [--tag <tag>] [--metadata <file>] | pick <dir> <suffix> <destination>');
}

main(process.argv.slice(2)).catch((error) => {
  console.error(`release: ${error.message}`);
  process.exitCode = error instanceof UsageError ? 2 : 1;
});
