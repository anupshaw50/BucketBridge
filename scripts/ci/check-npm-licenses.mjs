#!/usr/bin/env node
// Checks the licences of production npm dependencies against an allowlist.
//
// Usage:
//   pnpm --recursive licenses list --prod --json > npm-licenses.json
//   node scripts/ci/check-npm-licenses.mjs [--allow-empty] npm-licenses.json
//
// The input is the JSON printed by `pnpm licenses list --json`: an object that
// maps a licence string to the packages declaring it. A package passes when
// its licence is an SPDX expression that allowed licences satisfy: every
// operand of AND, at least one operand of OR, and for "X WITH Y" that exact
// pair. Anything else (unknown, custom, or unparsable licence information)
// fails unless the package is listed in EXCEPTIONS. An input without any
// package fails too, because that usually means the command ran in the wrong
// directory; pass --allow-empty when that is expected.

import { readFileSync } from 'node:fs';

// Same licences as [licenses] allow in deny.toml; keep the two in sync.
const ALLOWED = [
  '0BSD',
  'Apache-2.0',
  'Apache-2.0 WITH LLVM-exception',
  'BSD-2-Clause',
  'BSD-3-Clause',
  'BSL-1.0',
  'ISC',
  'MIT',
  'MIT-0',
  'Zlib',
  'Unicode-3.0',
  'MPL-2.0',
];

// Packages accepted although their licence string fails the rules above.
// `license` must equal the string pnpm reports, so that a licence change in a
// later release of the package fails the check again. Every entry needs a
// reason, for example:
//   { name: 'some-package', license: 'SEE LICENSE IN LICENSE.md', reason: 'MIT text in LICENSE.md' },
const EXCEPTIONS = [];

const normalize = (id) => id.replace(/\s+/g, ' ').trim().toLowerCase();
const ALLOWED_SET = new Set(ALLOWED.map(normalize));

class ParseError extends Error {}

// Grammar (SPDX annex D): or := and ("OR" and)*; and := with ("AND" with)*;
// with := atom ("WITH" exception)?; atom := "(" or ")" | licence-id
// Tokens are parentheses and runs of other non-space characters; a token that
// is not a valid identifier or operator is reported as unexpected.
function parse(expression) {
  const tokens = expression.match(/\(|\)|[^\s()]+/g) ?? [];
  let pos = 0;
  const peek = () => tokens[pos];
  const isOp = (token, op) => token !== undefined && token.toUpperCase() === op;
  const isId = (token) =>
    token !== undefined && /^[A-Za-z0-9.+:-]+$/.test(token) && !['AND', 'OR', 'WITH'].includes(token.toUpperCase());

  function parseOr() {
    const operands = [parseAnd()];
    while (isOp(peek(), 'OR')) {
      pos += 1;
      operands.push(parseAnd());
    }
    return operands.length === 1 ? operands[0] : { type: 'or', operands };
  }
  function parseAnd() {
    const operands = [parseWith()];
    while (isOp(peek(), 'AND')) {
      pos += 1;
      operands.push(parseWith());
    }
    return operands.length === 1 ? operands[0] : { type: 'and', operands };
  }
  function parseWith() {
    const node = parseAtom();
    if (!isOp(peek(), 'WITH')) return node;
    pos += 1;
    const exception = tokens[pos];
    if (node.type !== 'id' || !isId(exception)) throw new ParseError('malformed WITH clause');
    pos += 1;
    return { type: 'id', id: `${node.id} WITH ${exception}` };
  }
  function parseAtom() {
    const token = tokens[pos];
    if (token === '(') {
      pos += 1;
      const node = parseOr();
      if (tokens[pos] !== ')') throw new ParseError('missing closing parenthesis');
      pos += 1;
      return node;
    }
    if (!isId(token)) throw new ParseError(token === undefined ? 'unexpected end' : `unexpected token ${token}`);
    pos += 1;
    return { type: 'id', id: token };
  }

  const tree = parseOr();
  if (pos !== tokens.length) throw new ParseError(`unexpected token ${tokens[pos]}`);
  return tree;
}

function satisfied(node) {
  if (node.type === 'and') return node.operands.every(satisfied);
  if (node.type === 'or') return node.operands.some(satisfied);
  // "X+" means "X or any later version", so it is satisfied by X itself.
  const [base, ...rest] = node.id.split(' WITH ');
  const id = [base.replace(/\+$/, ''), ...rest].join(' WITH ');
  return ALLOWED_SET.has(normalize(id));
}

function evaluate(license) {
  if (typeof license !== 'string' || license.trim() === '') {
    return { ok: false, why: 'no licence information' };
  }
  try {
    return satisfied(parse(license.trim())) ? { ok: true } : { ok: false, why: 'not on the allowlist' };
  } catch (error) {
    if (error instanceof ParseError) return { ok: false, why: `not a valid SPDX expression (${error.message})` };
    throw error;
  }
}

function packagesOf(report) {
  if (report === null || typeof report !== 'object' || Array.isArray(report)) {
    throw new Error('expected a JSON object that maps licences to package lists');
  }
  const packages = [];
  for (const [key, list] of Object.entries(report)) {
    if (!Array.isArray(list)) throw new Error(`entry ${JSON.stringify(key)} is not a list of packages`);
    for (const pkg of list) {
      if (pkg === null || typeof pkg !== 'object' || typeof pkg.name !== 'string') {
        throw new Error(`entry ${JSON.stringify(key)} contains an item without a package name`);
      }
      const versions = Array.isArray(pkg.versions) ? pkg.versions : [pkg.version ?? '?'];
      packages.push({ name: pkg.name, versions, license: typeof pkg.license === 'string' ? pkg.license : key });
    }
  }
  return packages;
}

function main(args) {
  const allowEmpty = args.includes('--allow-empty');
  const files = args.filter((a) => a !== '--allow-empty');
  if (files.length !== 1 || files[0].startsWith('--')) {
    console.error('usage: node scripts/ci/check-npm-licenses.mjs [--allow-empty] <pnpm-licenses.json>');
    return 2;
  }

  const packages = packagesOf(JSON.parse(readFileSync(files[0], 'utf8')));
  if (packages.length === 0 && !allowEmpty) {
    console.error('check-npm-licenses: the report lists no packages; run `pnpm --recursive licenses list --prod --json` after `pnpm install`');
    return 1;
  }

  const counts = new Map();
  const failures = [];
  const usedExceptions = new Set();
  for (const pkg of packages) {
    counts.set(pkg.license, (counts.get(pkg.license) ?? 0) + 1);
    const result = evaluate(pkg.license);
    if (result.ok) continue;
    const exception = EXCEPTIONS.find((e) => e.name === pkg.name && e.license === pkg.license);
    if (exception) {
      usedExceptions.add(exception);
      continue;
    }
    failures.push(`${pkg.name}@${pkg.versions.join(',')}: ${JSON.stringify(pkg.license)} (${result.why})`);
  }

  console.log(`Production npm packages: ${packages.length}`);
  for (const [license, count] of [...counts].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))) {
    console.log(`  ${license}: ${count}`);
  }
  for (const exception of EXCEPTIONS) {
    if (!usedExceptions.has(exception)) {
      console.log(`warning: exception for ${exception.name} (${exception.license}) matched no package`);
    }
  }
  if (failures.length > 0) {
    console.error(`\n${failures.length} package(s) with licences outside the policy:`);
    for (const failure of failures) console.error(`  ${failure}`);
    console.error('\nReplace the dependency, or add a justified entry to EXCEPTIONS in scripts/ci/check-npm-licenses.mjs.');
    return 1;
  }
  console.log('All production npm licences are allowed.');
  return 0;
}

try {
  process.exitCode = main(process.argv.slice(2));
} catch (error) {
  console.error(`check-npm-licenses: ${error.message}`);
  process.exitCode = 1;
}
