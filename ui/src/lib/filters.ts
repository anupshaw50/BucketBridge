// A TypeScript port of the object filter engine of crates/core/src/filter.rs.
// The object list applies saved filters over the entries it has loaded, so the
// matching has to give the same answers as the engine the backend validates
// the filters with. The rules are documented in the module docs of filter.rs:
//
// - every path criterion looks at the "/"-separated path relative to the
//   listing prefix;
// - globs are matched on the UTF-8 bytes of the path, case-sensitively:
//   `*` and `?` never cross "/", `**` as a whole segment matches any number
//   of segments, `[abc]`, `[a-z]` and `[!abc]` never match "/", `{a,b}` are
//   alternatives and `\` escapes the next character;
// - `prefix` is a plain string prefix, size bounds are inclusive,
//   `modified_after` is inclusive and `modified_before` exclusive;
// - storage classes compare case-insensitively and a missing class is STANDARD;
// - folder entries match when something below them can satisfy the prefix.
//
// The glob parser below follows the parser of the `globset` crate the engine
// is built on, token by token, and the regex it produces matches what the
// engine compiles (classes never match "/"), but it runs over "binary
// strings" (one character per UTF-8 byte), as the Rust regex does over bytes.

import type { FilterSpec, ObjectEntry } from "../api/types";
import { formatBytes, formatDateTime, parseTimestamp } from "./format";

/** Storage class assumed for entries that do not report one. */
export const DEFAULT_STORAGE_CLASS = "STANDARD";

/** Longest regular expression a glob may compile to (the engine caps the size of the automaton too). */
const MAX_REGEX_LENGTH = 16_384;

const ENCODER = new TextEncoder();

/** Thrown inside the parser; turned into a message by `compileGlob`. */
class GlobSyntaxError extends Error {}

type Token =
  | { kind: "literal"; char: string }
  | { kind: "any" }
  | { kind: "zeroOrMore" }
  | { kind: "recursivePrefix" }
  | { kind: "recursiveSuffix" }
  | { kind: "recursiveZeroOrMore" }
  | { kind: "class"; negated: boolean; ranges: [string, string][] }
  | { kind: "alternates"; branches: Token[][] };

const UNCLOSED_CLASS = "unclosed character class; missing ']'";
const UNOPENED_ALTERNATES = "unopened alternate group; missing '{' (maybe escape '}' with '[}]'?)";
const UNCLOSED_ALTERNATES = "unclosed alternate group; missing '}' (maybe escape '{' with '[{]'?)";
const DANGLING_ESCAPE = "dangling '\\'";

function codePoint(char: string): number {
  return char.codePointAt(0) ?? 0;
}

/** Parses a glob into tokens, like `globset::GlobBuilder` with escapes on and empty alternates off. */
function parseGlob(pattern: string): Token[] {
  const chars = Array.from(pattern);
  let position = 0;
  let prev: string | null = null;
  let cur: string | null = null;
  const branches: Token[][] = [[]];
  const alternatesStack: number[] = [];

  const bump = (): string | null => {
    prev = cur;
    if (position < chars.length) {
      cur = chars[position] ?? null;
      position += 1;
    } else {
      cur = null;
    }
    return cur;
  };
  const peek = (): string | null => (position < chars.length ? (chars[position] ?? null) : null);
  const last = (): Token[] => {
    const branch = branches[branches.length - 1];
    if (branch === undefined) {
      throw new GlobSyntaxError(UNOPENED_ALTERNATES);
    }
    return branch;
  };
  const push = (token: Token): void => {
    last().push(token);
  };
  const pushTwice = (token: Token): void => {
    push(token);
    push(token);
  };
  const pop = (): Token => {
    const token = last().pop();
    if (token === undefined) {
      throw new GlobSyntaxError("invalid use of **; must be one path component");
    }
    return token;
  };

  const parseStar = (): void => {
    const before = prev;
    if (peek() !== "*") {
      push({ kind: "zeroOrMore" });
      return;
    }
    bump();
    if (last().length === 0) {
      const next = peek();
      if (next !== null && next !== "/") {
        pushTwice({ kind: "zeroOrMore" });
      } else {
        push({ kind: "recursivePrefix" });
        bump();
      }
      return;
    }
    if (before !== "/" && (branches.length <= 1 || (before !== "," && before !== "{"))) {
      pushTwice({ kind: "zeroOrMore" });
      return;
    }
    let isSuffix: boolean;
    const next = peek();
    if (next === null) {
      bump();
      isSuffix = true;
    } else if ((next === "," || next === "}") && branches.length >= 2) {
      isSuffix = true;
    } else if (next === "/") {
      bump();
      isSuffix = false;
    } else {
      pushTwice({ kind: "zeroOrMore" });
      return;
    }
    const popped = pop();
    if (popped.kind === "recursivePrefix" || popped.kind === "recursiveSuffix") {
      push(popped);
    } else {
      push({ kind: isSuffix ? "recursiveSuffix" : "recursiveZeroOrMore" });
    }
  };

  const parseClass = (): void => {
    const ranges: [string, string][] = [];
    const lead = peek();
    const negated = lead === "!" || lead === "^";
    if (negated) {
      bump();
    }
    let first = true;
    let inRange = false;
    const extend = (range: [string, string] | undefined, end: string): void => {
      if (range === undefined) {
        return;
      }
      range[1] = end;
      if (codePoint(range[1]) < codePoint(range[0])) {
        throw new GlobSyntaxError(`invalid range; '${range[0]}' > '${range[1]}'`);
      }
    };
    for (;;) {
      const c = bump();
      if (c === null) {
        throw new GlobSyntaxError(UNCLOSED_CLASS);
      }
      if (c === "]") {
        if (!first) {
          break;
        }
        ranges.push(["]", "]"]);
      } else if (c === "-") {
        if (first) {
          ranges.push(["-", "-"]);
        } else if (inRange) {
          extend(ranges[ranges.length - 1], "-");
          inRange = false;
        } else {
          inRange = true;
        }
      } else {
        if (inRange) {
          extend(ranges[ranges.length - 1], c);
        } else {
          ranges.push([c, c]);
        }
        inRange = false;
      }
      first = false;
    }
    if (inRange) {
      ranges.push(["-", "-"]);
    }
    push({ kind: "class", negated, ranges });
  };

  for (let c = bump(); c !== null; c = bump()) {
    switch (c) {
      case "?":
        push({ kind: "any" });
        break;
      case "*":
        parseStar();
        break;
      case "[":
        parseClass();
        break;
      case "{":
        alternatesStack.push(branches.length);
        branches.push([]);
        break;
      case "}": {
        const start = alternatesStack.pop();
        if (start === undefined) {
          throw new GlobSyntaxError(UNOPENED_ALTERNATES);
        }
        push({ kind: "alternates", branches: branches.splice(start) });
        break;
      }
      case ",":
        if (alternatesStack.length === 0) {
          push({ kind: "literal", char: "," });
        } else {
          branches.push([]);
        }
        break;
      case "\\": {
        const escaped = bump();
        if (escaped === null) {
          throw new GlobSyntaxError(DANGLING_ESCAPE);
        }
        push({ kind: "literal", char: escaped });
        break;
      }
      default:
        push({ kind: "literal", char: c });
    }
  }
  if (branches.length > 1) {
    throw new GlobSyntaxError(UNCLOSED_ALTERNATES);
  }
  return branches[0] ?? [];
}

/** A regular expression that matches exactly one byte. */
function byteRegex(byte: number): string {
  const alphanumeric =
    (byte >= 0x30 && byte <= 0x39) || (byte >= 0x41 && byte <= 0x5a) || (byte >= 0x61 && byte <= 0x7a);
  return alphanumeric ? String.fromCharCode(byte) : `\\x${byte.toString(16).padStart(2, "0")}`;
}

/** The UTF-8 bytes of a character, each as a regular expression. */
function literalRegex(char: string): string {
  return Array.from(ENCODER.encode(char), byteRegex).join("");
}

const SEPARATOR = byteRegex(0x2f);
const ANY_BYTE = "[\\s\\S]";

function tokensRegex(tokens: readonly Token[]): string {
  let out = "";
  for (const token of tokens) {
    switch (token.kind) {
      case "literal":
        out += literalRegex(token.char);
        break;
      case "any":
        out += `[^${SEPARATOR}]`;
        break;
      case "zeroOrMore":
        out += `[^${SEPARATOR}]*`;
        break;
      case "recursivePrefix":
        out += `(?:${SEPARATOR}?|${ANY_BYTE}*${SEPARATOR})`;
        break;
      case "recursiveSuffix":
        out += `${SEPARATOR}${ANY_BYTE}*`;
        break;
      case "recursiveZeroOrMore":
        out += `(?:${SEPARATOR}|${SEPARATOR}${ANY_BYTE}*${SEPARATOR})`;
        break;
      case "class": {
        const members = token.ranges
          .map(([from, to]) => (from === to ? literalRegex(from) : `${literalRegex(from)}-${literalRegex(to)}`))
          .join("");
        // A class never matches the separator, whatever its members are.
        out += token.negated ? `[^${SEPARATOR}${members}]` : `(?!${SEPARATOR})[${members}]`;
        break;
      }
      case "alternates": {
        const parts = token.branches.map((branch) => tokensRegex(branch)).filter((part) => part.length > 0);
        if (parts.length > 0) {
          out += `(?:${parts.join("|")})`;
        }
        break;
      }
    }
  }
  return out;
}

function isAscii(text: string): boolean {
  for (let index = 0; index < text.length; index += 1) {
    if (text.charCodeAt(index) > 0x7f) {
      return false;
    }
  }
  return true;
}

/** One character per UTF-8 byte, so that a RegExp works on bytes like the engine's regex does. */
function binaryString(text: string): string {
  if (isAscii(text)) {
    return text;
  }
  let out = "";
  for (const byte of ENCODER.encode(text)) {
    out += String.fromCharCode(byte);
  }
  return out;
}

/** A compiled glob. */
export interface Glob {
  readonly pattern: string;
  /** Whether the whole of `path` matches the pattern. */
  isMatch(path: string): boolean;
}

export type GlobResult = { ok: true; glob: Glob } | { ok: false; message: string };

/** Compiles `pattern`; an empty or malformed pattern is an error with the engine's wording. */
export function compileGlob(pattern: string): GlobResult {
  if (pattern.length === 0) {
    return { ok: false, message: "empty glob pattern" };
  }
  let source: string;
  try {
    const tokens = parseGlob(pattern);
    const everything = tokens.length === 1 && tokens[0]?.kind === "recursivePrefix";
    source = `^${everything ? `${ANY_BYTE}*` : tokensRegex(tokens)}$`;
  } catch (error) {
    const reason = error instanceof GlobSyntaxError ? error.message : "the glob cannot be compiled";
    return { ok: false, message: `error parsing glob '${pattern}': ${reason}` };
  }
  if (source.length > MAX_REGEX_LENGTH) {
    return { ok: false, message: `glob '${pattern}' cannot be compiled: the pattern is too large` };
  }
  let regex: RegExp;
  try {
    regex = new RegExp(source);
  } catch {
    return { ok: false, message: `glob '${pattern}' cannot be compiled` };
  }
  return { ok: true, glob: { pattern, isMatch: (path) => regex.test(binaryString(path)) } };
}

/** A compiled `FilterSpec`. */
export interface CompiledFilter {
  /** Key prefix relative to the listing prefix; `null` when unset (an empty string counts as unset). */
  readonly prefix: string | null;
  readonly glob: Glob | null;
  readonly minSize: number | null;
  readonly maxSize: number | null;
  /** Epoch milliseconds, inclusive. */
  readonly modifiedAfter: number | null;
  /** Epoch milliseconds, exclusive. */
  readonly modifiedBefore: number | null;
  /** Trimmed and upper-cased, never empty strings. */
  readonly storageClasses: readonly string[];
}

export type CompileResult = { ok: true; filter: CompiledFilter } | { ok: false; message: string };

function wholeBytes(name: string, value: number | null): string | null {
  if (value !== null && (!Number.isSafeInteger(value) || value < 0)) {
    return `${name} must be a whole number of bytes, 0 or more`;
  }
  return null;
}

function parseBound(name: string, value: string | null): { ok: true; ms: number | null } | { ok: false; message: string } {
  if (value === null || value === "") {
    return { ok: true, ms: null };
  }
  const ms = parseTimestamp(value);
  if (ms === null) {
    return { ok: false, message: `${name} is not a valid RFC 3339 time` };
  }
  return { ok: true, ms };
}

/** Validates and compiles `spec`, with the checks (and the wording) of `Filter::compile`. */
export function compileFilter(spec: FilterSpec): CompileResult {
  const prefix = spec.prefix !== null && spec.prefix.length > 0 ? spec.prefix : null;
  let glob: Glob | null = null;
  if (spec.glob !== null && spec.glob.length > 0) {
    const compiled = compileGlob(spec.glob);
    if (!compiled.ok) {
      return compiled;
    }
    glob = compiled.glob;
  }
  for (const [name, value] of [
    ["min_size", spec.min_size],
    ["max_size", spec.max_size],
  ] as const) {
    const problem = wholeBytes(name, value);
    if (problem !== null) {
      return { ok: false, message: problem };
    }
  }
  if (spec.min_size !== null && spec.max_size !== null && spec.min_size > spec.max_size) {
    return { ok: false, message: `min_size (${spec.min_size}) is larger than max_size (${spec.max_size})` };
  }
  const after = parseBound("modified_after", spec.modified_after);
  if (!after.ok) {
    return after;
  }
  const before = parseBound("modified_before", spec.modified_before);
  if (!before.ok) {
    return before;
  }
  if (after.ms !== null && before.ms !== null && after.ms >= before.ms) {
    return { ok: false, message: "modified_after must be earlier than modified_before (the interval is empty)" };
  }
  const storageClasses: string[] = [];
  for (const raw of spec.storage_classes) {
    const name = raw.trim();
    if (name.length === 0) {
      return { ok: false, message: "storage_classes contains an empty name" };
    }
    storageClasses.push(name.toUpperCase());
  }
  return {
    ok: true,
    filter: {
      prefix,
      glob,
      minSize: spec.min_size,
      maxSize: spec.max_size,
      modifiedAfter: after.ms,
      modifiedBefore: before.ms,
      storageClasses,
    },
  };
}

/** A spec that accepts everything. */
export function emptyFilterSpec(): FilterSpec {
  return {
    prefix: null,
    glob: null,
    min_size: null,
    max_size: null,
    modified_after: null,
    modified_before: null,
    storage_classes: [],
  };
}

/** Whether the filter has a prefix or a glob. */
export function hasPathCriteria(filter: CompiledFilter): boolean {
  return filter.prefix !== null || filter.glob !== null;
}

/** Whether the filter has size, date or storage-class criteria. */
export function hasMetadataCriteria(filter: CompiledFilter): boolean {
  return (
    filter.minSize !== null ||
    filter.maxSize !== null ||
    filter.modifiedAfter !== null ||
    filter.modifiedBefore !== null ||
    filter.storageClasses.length > 0
  );
}

/** True when the filter accepts everything. */
export function isUnrestricted(filter: CompiledFilter): boolean {
  return !hasPathCriteria(filter) && !hasMetadataCriteria(filter);
}

/** Prefix and glob criteria only, on a path relative to the listing prefix. */
export function matchesPath(filter: CompiledFilter, relPath: string): boolean {
  return (
    (filter.prefix === null || relPath.startsWith(filter.prefix)) &&
    (filter.glob === null || filter.glob.isMatch(relPath))
  );
}

/**
 * Whether anything below the folder `dirRelPath` (relative, without a trailing "/"; empty for the
 * root) can satisfy the prefix criterion. Globs are not used for pruning.
 */
export function mayMatchBelow(filter: CompiledFilter, dirRelPath: string): boolean {
  if (filter.prefix === null || dirRelPath === "") {
    return true;
  }
  const dir = `${dirRelPath}/`;
  return filter.prefix.startsWith(dir) || dir.startsWith(filter.prefix);
}

/** Size, date and storage-class criteria only; a missing class is STANDARD, a missing time never satisfies a date bound. */
export function matchesMetadata(
  filter: CompiledFilter,
  size: number,
  modified: number | null,
  storageClass: string | null,
): boolean {
  if (filter.minSize !== null && size < filter.minSize) {
    return false;
  }
  if (filter.maxSize !== null && size > filter.maxSize) {
    return false;
  }
  if (filter.modifiedAfter !== null || filter.modifiedBefore !== null) {
    if (modified === null) {
      return false;
    }
    if (filter.modifiedAfter !== null && modified < filter.modifiedAfter) {
      return false;
    }
    if (filter.modifiedBefore !== null && modified >= filter.modifiedBefore) {
      return false;
    }
  }
  if (filter.storageClasses.length > 0) {
    const trimmed = storageClass?.trim() ?? "";
    const wanted = (trimmed.length > 0 ? trimmed : DEFAULT_STORAGE_CLASS).toUpperCase();
    if (!filter.storageClasses.includes(wanted)) {
      return false;
    }
  }
  return true;
}

/**
 * Evaluates every criterion on a listing entry whose key is relative to `listingPrefix`. Entries
 * outside `listingPrefix` never match. Folder entries (a common prefix, or a key ending in "/") have
 * no size, date or class of their own: they match when something below them can satisfy the prefix
 * criterion, so a filtered folder view stays navigable.
 */
export function matchesObject(filter: CompiledFilter, entry: ObjectEntry, listingPrefix: string): boolean {
  if (!entry.key.startsWith(listingPrefix)) {
    return false;
  }
  const relPath = entry.key.slice(listingPrefix.length);
  if (entry.is_prefix || relPath.endsWith("/")) {
    return mayMatchBelow(filter, relPath.replace(/\/+$/, ""));
  }
  return (
    matchesPath(filter, relPath) &&
    matchesMetadata(filter, entry.size, parseTimestamp(entry.last_modified), entry.storage_class)
  );
}

/** A date at UTC midnight is shown as the day, any other time in full. */
function describeTime(value: string): string {
  return /T00:00:00(\.0+)?(Z|z|\+00:00)$/.test(value) ? value.slice(0, 10) : formatDateTime(value);
}

/** A one-line description of what a filter selects, for lists and tooltips. */
export function describeFilter(spec: FilterSpec): string {
  const parts: string[] = [];
  if (spec.prefix !== null && spec.prefix.length > 0) {
    parts.push(`prefix "${spec.prefix}"`);
  }
  if (spec.glob !== null && spec.glob.length > 0) {
    parts.push(`glob ${spec.glob}`);
  }
  if (spec.min_size !== null && spec.max_size !== null) {
    parts.push(`size ${formatBytes(spec.min_size)} to ${formatBytes(spec.max_size)}`);
  } else if (spec.min_size !== null) {
    parts.push(`size at least ${formatBytes(spec.min_size)}`);
  } else if (spec.max_size !== null) {
    parts.push(`size at most ${formatBytes(spec.max_size)}`);
  }
  if (spec.modified_after !== null) {
    parts.push(`modified on or after ${describeTime(spec.modified_after)}`);
  }
  if (spec.modified_before !== null) {
    parts.push(`modified before ${describeTime(spec.modified_before)}`);
  }
  if (spec.storage_classes.length > 0) {
    parts.push(`class ${spec.storage_classes.join(", ")}`);
  }
  return parts.length > 0 ? parts.join("; ") : "Matches everything";
}
