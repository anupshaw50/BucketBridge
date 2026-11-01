#!/usr/bin/env node
// Creates a release archive (.zip or .tar.gz) with Node's standard library
// only, so the same code runs on the Windows, macOS, and Linux runners.
//
// Usage: node scripts/ci/archive.mjs <output.zip|output.tar.gz> <name=path>...
//
// Each <name=path> argument adds the file at <path> to the archive as
// <folder>/<name>, where <folder> is the output file name without its
// extension. Entries keep their modification time; a file is stored with
// mode 755 when any execute bit is set on it and 644 otherwise. The output
// file must not exist yet.

import { mkdir, readFile, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import zlib from 'node:zlib';

const MAX_32 = 0xffffffff;

class UsageError extends Error {}

function parseArgs(args) {
  if (args.length < 2) {
    throw new UsageError('usage: node scripts/ci/archive.mjs <output.zip|output.tar.gz> <name=path>...');
  }
  const [output, ...specs] = args;
  const base = path.basename(output);
  let format;
  let folder;
  if (base.toLowerCase().endsWith('.zip')) {
    format = 'zip';
    folder = base.slice(0, -'.zip'.length);
  } else if (base.toLowerCase().endsWith('.tar.gz')) {
    format = 'tar.gz';
    folder = base.slice(0, -'.tar.gz'.length);
  } else {
    throw new UsageError(`unsupported archive type: ${output} (use .zip or .tar.gz)`);
  }
  if (folder === '' || folder.startsWith('.')) {
    throw new UsageError(`cannot derive a folder name from ${output}`);
  }

  const entries = [];
  const seen = new Set();
  for (const spec of specs) {
    const eq = spec.indexOf('=');
    if (eq <= 0 || eq === spec.length - 1) {
      throw new UsageError(`expected <name>=<path>, got ${JSON.stringify(spec)}`);
    }
    const name = spec.slice(0, eq);
    const source = spec.slice(eq + 1);
    if (/[/\\]/.test(name) || name === '.' || name === '..') {
      throw new UsageError(`entry name must be a plain file name: ${JSON.stringify(name)}`);
    }
    if (seen.has(name)) {
      throw new UsageError(`duplicate entry name: ${name}`);
    }
    seen.add(name);
    entries.push({ name: `${folder}/${name}`, source });
  }
  return { output, format, folder, entries };
}

async function loadEntries(entries) {
  for (const entry of entries) {
    const info = await stat(entry.source);
    if (!info.isFile()) {
      throw new Error(`${entry.source} is not a regular file`);
    }
    entry.data = await readFile(entry.source);
    entry.mode = info.mode & 0o111 ? 0o755 : 0o644;
    entry.mtime = info.mtime;
  }
}

// ---- tar (POSIX ustar) ----

function writeString(buffer, offset, length, value) {
  const bytes = Buffer.from(value, 'utf8');
  if (bytes.length > length) {
    throw new Error(`tar header field too long: ${value}`);
  }
  bytes.copy(buffer, offset);
}

function writeOctal(buffer, offset, length, value) {
  const digits = value.toString(8);
  if (digits.length > length - 1) {
    throw new Error(`value ${value} does not fit a ${length}-byte tar field`);
  }
  writeString(buffer, offset, length, `${digits.padStart(length - 1, '0')}\0`);
}

function tarHeader({ name, mode, size, mtime, type }) {
  if (Buffer.byteLength(name, 'utf8') > 100) {
    throw new Error(`path too long for a tar header: ${name}`);
  }
  const header = Buffer.alloc(512, 0);
  writeString(header, 0, 100, name);
  writeOctal(header, 100, 8, mode);
  writeOctal(header, 108, 8, 0); // uid
  writeOctal(header, 116, 8, 0); // gid
  writeOctal(header, 124, 12, size);
  writeOctal(header, 136, 12, Math.floor(mtime.getTime() / 1000));
  header.fill(0x20, 148, 156); // checksum field counts as spaces while summing
  writeString(header, 156, 1, type);
  writeString(header, 257, 6, 'ustar\0');
  writeString(header, 263, 2, '00');
  let checksum = 0;
  for (const byte of header) checksum += byte;
  writeString(header, 148, 8, `${checksum.toString(8).padStart(6, '0')}\0 `);
  return header;
}

function tarGz(folder, entries) {
  const latest = new Date(Math.max(...entries.map((e) => e.mtime.getTime())));
  const parts = [tarHeader({ name: `${folder}/`, mode: 0o755, size: 0, mtime: latest, type: '5' })];
  for (const entry of entries) {
    parts.push(tarHeader({ name: entry.name, mode: entry.mode, size: entry.data.length, mtime: entry.mtime, type: '0' }));
    parts.push(entry.data);
    const padding = (512 - (entry.data.length % 512)) % 512;
    if (padding > 0) parts.push(Buffer.alloc(padding, 0));
  }
  parts.push(Buffer.alloc(1024, 0)); // end-of-archive marker: two zero blocks
  return zlib.gzipSync(Buffer.concat(parts), { level: 9 });
}

// ---- zip ----

function dosDateTime(date) {
  const year = date.getFullYear();
  if (year < 1980) return { time: 0, date: (1 << 5) | 1 }; // 1980-01-01 00:00
  const clampedYear = Math.min(year, 2107);
  return {
    time: (date.getHours() << 11) | (date.getMinutes() << 5) | Math.floor(date.getSeconds() / 2),
    date: ((clampedYear - 1980) << 9) | ((date.getMonth() + 1) << 5) | date.getDate(),
  };
}

function zip(folder, entries) {
  if (typeof zlib.crc32 !== 'function') {
    throw new Error('zlib.crc32 is unavailable; use Node.js 22.2 or newer');
  }
  const latest = new Date(Math.max(...entries.map((e) => e.mtime.getTime())));
  const items = [
    { name: `${folder}/`, data: Buffer.alloc(0), mtime: latest, attributes: ((0o040755 << 16) | 0x10) >>> 0, directory: true },
    ...entries.map((e) => ({ name: e.name, data: e.data, mtime: e.mtime, attributes: ((0o100000 | e.mode) << 16) >>> 0, directory: false })),
  ];
  if (items.length > 0xffff) throw new Error('too many entries for a zip without ZIP64');

  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const item of items) {
    const name = Buffer.from(item.name, 'utf8');
    const flags = /[^\x20-\x7e]/.test(item.name) ? 0x0800 : 0; // bit 11: UTF-8 name
    const crc = zlib.crc32(item.data) >>> 0;
    let method = 0;
    let body = item.data;
    if (!item.directory && item.data.length > 0) {
      const deflated = zlib.deflateRawSync(item.data, { level: 9 });
      if (deflated.length < item.data.length) {
        method = 8;
        body = deflated;
      }
    }
    if (item.data.length > MAX_32 || body.length > MAX_32 || offset > MAX_32) {
      throw new Error(`${item.name} is too large for a zip without ZIP64`);
    }
    const { time, date } = dosDateTime(item.mtime);

    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4); // version needed to extract: 2.0
    local.writeUInt16LE(flags, 6);
    local.writeUInt16LE(method, 8);
    local.writeUInt16LE(time, 10);
    local.writeUInt16LE(date, 12);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(body.length, 18);
    local.writeUInt32LE(item.data.length, 22);
    local.writeUInt16LE(name.length, 26);
    local.writeUInt16LE(0, 28); // extra field length
    locals.push(local, name, body);

    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE((3 << 8) | 20, 4); // made by: Unix, spec 2.0
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(flags, 8);
    central.writeUInt16LE(method, 10);
    central.writeUInt16LE(time, 12);
    central.writeUInt16LE(date, 14);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(body.length, 20);
    central.writeUInt32LE(item.data.length, 24);
    central.writeUInt16LE(name.length, 28);
    central.writeUInt16LE(0, 30); // extra field length
    central.writeUInt16LE(0, 32); // comment length
    central.writeUInt16LE(0, 34); // disk number start
    central.writeUInt16LE(0, 36); // internal attributes
    central.writeUInt32LE(item.attributes, 38);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, name);

    offset += local.length + name.length + body.length;
  }

  const centralDirectory = Buffer.concat(centrals);
  if (offset > MAX_32 || centralDirectory.length > MAX_32) {
    throw new Error('archive too large for a zip without ZIP64');
  }
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(0, 4); // number of this disk
  end.writeUInt16LE(0, 6); // disk where the central directory starts
  end.writeUInt16LE(items.length, 8);
  end.writeUInt16LE(items.length, 10);
  end.writeUInt32LE(centralDirectory.length, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(0, 20); // comment length
  return Buffer.concat([...locals, centralDirectory, end]);
}

async function main(args) {
  const { output, format, folder, entries } = parseArgs(args);
  await loadEntries(entries);
  const archive = format === 'zip' ? zip(folder, entries) : tarGz(folder, entries);
  await mkdir(path.dirname(path.resolve(output)), { recursive: true });
  await writeFile(output, archive, { flag: 'wx' });
  console.log(`${output}: ${entries.length} file(s) under ${folder}/, ${archive.length} bytes`);
}

main(process.argv.slice(2)).catch((error) => {
  console.error(`archive: ${error.message}`);
  process.exitCode = error instanceof UsageError ? 2 : 1;
});
