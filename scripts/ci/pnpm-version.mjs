#!/usr/bin/env node
// Prints the `version` input for pnpm/action-setup as a `version=<value>` line
// for $GITHUB_OUTPUT.
//
// pnpm/action-setup fails when its `version` input and the root package.json
// both pin pnpm to different values. This prints an empty value when the root
// package.json pins pnpm (the `packageManager` field, or a pnpm entry in
// `devEngines.packageManager`), so that the action uses that pin, and the
// fallback given on the command line otherwise.
//
// Usage: node scripts/ci/pnpm-version.mjs <fallback-version> >> "$GITHUB_OUTPUT"
// Run it from the repository root. Uses only syntax that the Node.js versions
// preinstalled on GitHub-hosted runners understand.

import { readFileSync } from 'node:fs';

const fallback = process.argv[2];
if (!fallback || process.argv.length !== 3) {
  console.error('usage: node scripts/ci/pnpm-version.mjs <fallback-version>');
  process.exit(2);
}

const manifest = JSON.parse(readFileSync('package.json', 'utf8'));
const packageManager = manifest.packageManager;
const devEngine = manifest.devEngines && manifest.devEngines.packageManager;

const pinnedByPackageManager = typeof packageManager === 'string' && packageManager.startsWith('pnpm@');
const pinnedByDevEngines =
  devEngine !== null &&
  typeof devEngine === 'object' &&
  !Array.isArray(devEngine) &&
  devEngine.name === 'pnpm' &&
  typeof devEngine.version === 'string' &&
  devEngine.version !== '';

process.stdout.write(`version=${pinnedByPackageManager || pinnedByDevEngines ? '' : fallback}\n`);
