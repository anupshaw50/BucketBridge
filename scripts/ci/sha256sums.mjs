#!/usr/bin/env node
// Writes <dir>/SHA256SUMS covering every file in <dir>, in the format that
// `sha256sum --check` and `shasum -a 256 --check` read. Lines are sorted by
// file name. The directory must be flat: subdirectories, symlinks, and other
// non-regular entries are rejected instead of being skipped silently.
//
// Usage: node scripts/ci/sha256sums.mjs <dir>

import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';

const OUTPUT = 'SHA256SUMS';

async function sha256(file) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(file)) {
    hash.update(chunk);
  }
  return hash.digest('hex');
}

async function main(args) {
  if (args.length !== 1) {
    console.error('usage: node scripts/ci/sha256sums.mjs <dir>');
    return 2;
  }
  const dir = args[0];
  const entries = await readdir(dir, { withFileTypes: true });
  const names = [];
  for (const entry of entries) {
    if (entry.name === OUTPUT) continue;
    if (!entry.isFile()) {
      throw new Error(`${path.join(dir, entry.name)} is not a regular file`);
    }
    // The checksum format escapes these characters; release artefacts never need them.
    if (/[\\\r\n]/.test(entry.name)) {
      throw new Error(`unsupported character in file name ${JSON.stringify(entry.name)}`);
    }
    names.push(entry.name);
  }
  if (names.length === 0) {
    throw new Error(`no files to checksum in ${dir}`);
  }
  names.sort();

  let sums = '';
  for (const name of names) {
    sums += `${await sha256(path.join(dir, name))}  ${name}\n`;
  }
  await writeFile(path.join(dir, OUTPUT), sums);
  process.stdout.write(sums);
  return 0;
}

main(process.argv.slice(2)).then(
  (code) => {
    process.exitCode = code;
  },
  (error) => {
    console.error(`sha256sums: ${error.message}`);
    process.exitCode = 1;
  },
);
