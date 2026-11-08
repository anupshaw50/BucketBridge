export const MAX_INVALIDATION_PATHS = 3000;
export const MAX_PATH_BYTES = 4000;

import { tr } from "../../i18n";

export type PathCheck = { ok: true; paths: string[] } | { ok: false; message: string };

/** The problem with one trimmed, non-empty path, or null when it is valid. */
function problemWith(path: string): string | null {
  if (!path.startsWith("/")) {
    return tr("a path must start with '/'");
  }
  if (new TextEncoder().encode(path).length > MAX_PATH_BYTES) {
    return tr("longer than {n} bytes", { n: MAX_PATH_BYTES });
  }
  if ([...path].some((char) => /\s/u.test(char) || char.charCodeAt(0) < 0x20 || char.charCodeAt(0) === 0x7f)) {
    return tr("blanks and control characters must be percent-encoded (%20)");
  }
  const star = path.indexOf("*");
  if (star >= 0 && star !== path.length - 1) {
    return tr("'*' is only allowed as the last character");
  }
  return null;
}

/**
 * Checks invalidation paths like the core does: each starts with "/", "*" only as the last character, no blanks,
 * 1 to 3000 paths. Returns them trimmed and without repeats, or every problem found.
 */
export function validatePaths(paths: readonly string[]): PathCheck {
  const problems: string[] = [];
  const checked: string[] = [];
  paths.forEach((raw, index) => {
    const path = raw.trim();
    if (path.length === 0) {
      problems.push(tr("path {n} is empty", { n: index + 1 }));
      return;
    }
    const problem = problemWith(path);
    if (problem !== null) {
      problems.push(tr("'{path}': {problem}", { path, problem }));
    } else if (!checked.includes(path)) {
      checked.push(path);
    }
  });
  if (checked.length === 0 && problems.length === 0) {
    problems.push(tr("give at least one path"));
  }
  if (checked.length > MAX_INVALIDATION_PATHS) {
    problems.push(tr("{n} paths; one invalidation holds at most {max}", { n: checked.length, max: MAX_INVALIDATION_PATHS }));
  }
  return problems.length === 0 ? { ok: true, paths: checked } : { ok: false, message: tr("invalid invalidation paths: {problems}", { problems: problems.join("; ") }) };
}

/** The lines of a text box as paths: one per line, blank lines dropped. */
export function pathsFromText(text: string): string[] {
  return text
    .split(/\r?\n/u)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}
