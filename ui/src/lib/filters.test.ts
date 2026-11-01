import { describe, expect, it } from "vitest";
import type { FilterSpec, ObjectEntry } from "../api/types";
import {
  type CompiledFilter,
  compileFilter,
  compileGlob,
  emptyFilterSpec,
  hasMetadataCriteria,
  hasPathCriteria,
  isUnrestricted,
  matchesMetadata,
  matchesObject,
  matchesPath,
  mayMatchBelow,
} from "./filters";
import { parseTimestamp } from "./format";

// These tests mirror the tests of crates/core/src/filter.rs: the TypeScript port has to give the
// same answers as the engine that validates the saved filters.

function spec(patch: Partial<FilterSpec>): FilterSpec {
  return { ...emptyFilterSpec(), ...patch };
}

function compiled(patch: Partial<FilterSpec>): CompiledFilter {
  const result = compileFilter(spec(patch));
  if (!result.ok) {
    throw new Error(result.message);
  }
  return result.filter;
}

function globFilter(glob: string): CompiledFilter {
  return compiled({ glob });
}

function ts(year: number, month: number, day: number): number {
  return Date.UTC(year, month - 1, day);
}

function iso(year: number, month: number, day: number): string {
  return new Date(ts(year, month, day)).toISOString();
}

function object(key: string, size: number, modified: string | null, storageClass: string | null): ObjectEntry {
  return {
    key,
    size,
    etag: null,
    last_modified: modified,
    storage_class: storageClass,
    checksums: { crc32: null, crc32c: null, crc64nvme: null, sha1: null, sha256: null },
    is_prefix: false,
  };
}

function folder(key: string): ObjectEntry {
  return { ...object(key, 0, null, null), is_prefix: true };
}

describe("globs", () => {
  it("star does not cross the separator but double star does", () => {
    const top = globFilter("*.jpg");
    expect(matchesPath(top, "a.jpg")).toBe(true);
    expect(matchesPath(top, ".jpg")).toBe(true);
    expect(matchesPath(top, "2024/a.jpg")).toBe(false);
    expect(matchesPath(top, "a.jpeg")).toBe(false);

    const deep = globFilter("**/*.jpg");
    expect(matchesPath(deep, "a.jpg")).toBe(true);
    expect(matchesPath(deep, "2024/a.jpg")).toBe(true);
    expect(matchesPath(deep, "2024/06/a.jpg")).toBe(true);
    expect(matchesPath(deep, "2024/06/a.png")).toBe(false);

    const below = globFilter("raw/**");
    expect(matchesPath(below, "raw/a")).toBe(true);
    expect(matchesPath(below, "raw/a/b/c")).toBe(true);
    expect(matchesPath(below, "raw")).toBe(false);
    expect(matchesPath(below, "rawx/a")).toBe(false);

    const middle = globFilter("a/**/z.txt");
    expect(matchesPath(middle, "a/z.txt")).toBe(true);
    expect(matchesPath(middle, "a/b/c/z.txt")).toBe(true);
    expect(matchesPath(middle, "b/z.txt")).toBe(false);

    const everything = globFilter("**");
    expect(matchesPath(everything, "x")).toBe(true);
    expect(matchesPath(everything, "x/y/z")).toBe(true);
  });

  it("question mark, classes, alternates and escapes", () => {
    const one = globFilter("a?c");
    expect(matchesPath(one, "abc")).toBe(true);
    expect(matchesPath(one, "a/c")).toBe(false);
    expect(matchesPath(one, "ac")).toBe(false);

    const cls = globFilter("[ab].txt");
    expect(matchesPath(cls, "a.txt")).toBe(true);
    expect(matchesPath(cls, "c.txt")).toBe(false);
    const negated = globFilter("[!ab].txt");
    expect(matchesPath(negated, "c.txt")).toBe(true);
    expect(matchesPath(negated, "a.txt")).toBe(false);

    const alternates = globFilter("*.{jpg,png}");
    expect(matchesPath(alternates, "a.jpg")).toBe(true);
    expect(matchesPath(alternates, "a.png")).toBe(true);
    expect(matchesPath(alternates, "a.gif")).toBe(false);

    const escaped = globFilter("\\*.txt");
    expect(matchesPath(escaped, "*.txt")).toBe(true);
    expect(matchesPath(escaped, "a.txt")).toBe(false);
  });

  it("classes never match the separator", () => {
    const negated = globFilter("a[!x]b");
    expect(matchesPath(negated, "acb")).toBe(true);
    expect(matchesPath(negated, "axb")).toBe(false);
    expect(matchesPath(negated, "a/b")).toBe(false);
    const caret = globFilter("a[^x]b");
    expect(matchesPath(caret, "acb")).toBe(true);
    expect(matchesPath(caret, "a/b")).toBe(false);
    // A range that spans "/" (0x2B to 0x30) does not match it either.
    const range = globFilter("a[+-0]b");
    expect(matchesPath(range, "a+b")).toBe(true);
    expect(matchesPath(range, "a0b")).toBe(true);
    expect(matchesPath(range, "a/b")).toBe(false);
    expect(matchesPath(globFilter("a[/]b"), "a/b")).toBe(false);
    // Escaped brackets and classes next to other tokens keep working.
    expect(matchesPath(globFilter("\\[a\\].txt"), "[a].txt")).toBe(true);
    expect(matchesPath(globFilter("[]x].txt"), "].txt")).toBe(true);
    expect(matchesPath(globFilter("**/[!_]*.txt"), "d/e/a.txt")).toBe(true);
    expect(matchesPath(globFilter("**/[!_]*.txt"), "d/_a.txt")).toBe(false);
    expect(matchesPath(globFilter("d[!a-c]/f"), "dz/f")).toBe(true);
    expect(matchesPath(globFilter("d[!a-c]/f"), "d/f")).toBe(false);
    // A non-ASCII member is matched byte-wise, as documented for classes.
    expect(matchesPath(globFilter("[!é]x"), "zx")).toBe(true);
  });

  it("is case sensitive and matches bytes, not characters", () => {
    const upper = globFilter("*.JPG");
    expect(matchesPath(upper, "A.JPG")).toBe(true);
    expect(matchesPath(upper, "a.jpg")).toBe(false);

    const star = globFilter("caf*.txt");
    expect(matchesPath(star, "café.txt")).toBe(true);
    // `?` is one byte, so it does not match the two-byte "e with acute accent".
    const question = globFilter("caf?.txt");
    expect(matchesPath(question, "café.txt")).toBe(false);
    expect(matchesPath(question, "cafe.txt")).toBe(true);
    // Two of them do.
    expect(matchesPath(globFilter("caf??.txt"), "café.txt")).toBe(true);
    // Literal non-ASCII characters in the pattern match themselves.
    expect(matchesPath(globFilter("café*"), "café.txt")).toBe(true);
    expect(matchesPath(globFilter("\u{1F4F7}/*"), "\u{1F4F7}/pic.jpg")).toBe(true);

    expect(matchesPath(globFilter("*"), "line\nbreak")).toBe(true);
  });

  it("never treats a backslash in a key as a separator", () => {
    const filter = globFilter("dir/*");
    expect(matchesPath(filter, "dir/a.txt")).toBe(true);
    expect(matchesPath(filter, "dir\\a.txt")).toBe(false);
    expect(matchesPath(globFilter("*"), "dir\\a.txt")).toBe(true);
    expect(matchesPath(globFilter("dir\\\\a.txt"), "dir\\a.txt")).toBe(true);
  });

  it("rejects invalid globs with the engine's wording", () => {
    for (const bad of ["[a", "a{b", "a\\"]) {
      const result = compileFilter(spec({ glob: bad }));
      expect(result.ok, bad).toBe(false);
    }
    expect(compileGlob("")).toEqual({ ok: false, message: "empty glob pattern" });
    expect(compileGlob("[a")).toEqual({
      ok: false,
      message: "error parsing glob '[a': unclosed character class; missing ']'",
    });
    expect(compileGlob("a\\")).toEqual({ ok: false, message: "error parsing glob 'a\\': dangling '\\'" });
    expect(compileGlob("[z-a]")).toEqual({ ok: false, message: "error parsing glob '[z-a]': invalid range; 'z' > 'a'" });
    for (const pattern of ["[", "[]", "[!", "[!]", "{a,b", "{a,{b,c}", "a,b}", "{a,b}}"]) {
      expect(compileGlob(pattern).ok, pattern).toBe(false);
    }
  });

  it("keeps globset's rules for alternates, commas and stars that are not whole segments", () => {
    // A comma outside an alternation is a plain character.
    expect(matchesPath(globFilter("a,b"), "a,b")).toBe(true);
    // Nested alternates are allowed in this globset version.
    const nested = globFilter("{a,{b,c}}.txt");
    expect(matchesPath(nested, "a.txt")).toBe(true);
    expect(matchesPath(nested, "c.txt")).toBe(true);
    expect(matchesPath(nested, "d.txt")).toBe(false);
    // An empty branch is dropped (empty alternates are off).
    const empty = globFilter("foo{,.txt}");
    expect(matchesPath(empty, "foo.txt")).toBe(true);
    expect(matchesPath(empty, "foo")).toBe(false);
    // `**` that is not a whole path segment behaves like `*`.
    const notWhole = globFilter("a**b");
    expect(matchesPath(notWhole, "axxb")).toBe(true);
    expect(matchesPath(notWhole, "a/b")).toBe(false);
    expect(matchesPath(globFilter("**foo"), "x/foo")).toBe(false);
    expect(matchesPath(globFilter("**foo"), "xfoo")).toBe(true);
    // `**` inside an alternation, right after the brace.
    const inBraces = globFilter("{**/a,b}");
    expect(matchesPath(inBraces, "x/y/a")).toBe(true);
    expect(matchesPath(inBraces, "b")).toBe(true);
    // A trailing `/**` and a middle `/**/`.
    expect(matchesPath(globFilter("a/**"), "a")).toBe(false);
    expect(matchesPath(globFilter("a/**/b/**"), "a/x/b/y")).toBe(true);
  });

  it("does not hang on a pathological pattern and refuses an oversized one", () => {
    const filter = globFilter("*a*a*a*a*a*a*b");
    expect(matchesPath(filter, `${"a".repeat(30)}c`)).toBe(false);
    const huge = compileGlob(`{${"abcdefgh,".repeat(3000)}z}`);
    expect(huge.ok).toBe(false);
  });
});

describe("filter specs", () => {
  it("counts empty strings as unset", () => {
    const filter = compiled({ prefix: "", glob: "" });
    expect(isUnrestricted(filter)).toBe(true);
    expect(matchesPath(filter, "anything/at/all")).toBe(true);
    expect(filter.prefix).toBeNull();
    expect(filter.glob).toBeNull();
  });

  it("treats the prefix as a plain string prefix", () => {
    const filter = compiled({ prefix: "20" });
    expect(matchesPath(filter, "2024/a.jpg")).toBe(true);
    expect(matchesPath(filter, "2025")).toBe(true);
    expect(matchesPath(filter, "1999/a.jpg")).toBe(false);
    expect(matchesPath(filter, "x/2024/a.jpg")).toBe(false);
    expect(hasPathCriteria(filter)).toBe(true);
    expect(hasMetadataCriteria(filter)).toBe(false);
  });

  it("prunes folders by the prefix only", () => {
    const filter = compiled({ prefix: "photos/2024/" });
    expect(mayMatchBelow(filter, "")).toBe(true);
    expect(mayMatchBelow(filter, "photos")).toBe(true);
    expect(mayMatchBelow(filter, "photos/2024")).toBe(true);
    expect(mayMatchBelow(filter, "photos/2024/06")).toBe(true);
    expect(mayMatchBelow(filter, "photos/2023")).toBe(false);
    expect(mayMatchBelow(filter, "docs")).toBe(false);
    expect(mayMatchBelow(filter, "photos/20")).toBe(false);
    const partial = compiled({ prefix: "pho" });
    expect(mayMatchBelow(partial, "photos")).toBe(true);
    expect(mayMatchBelow(partial, "docs")).toBe(false);
    expect(mayMatchBelow(globFilter("*.jpg"), "anything")).toBe(true);
  });

  it("makes size bounds inclusive", () => {
    const filter = compiled({ min_size: 10, max_size: 20 });
    expect(matchesMetadata(filter, 9, null, null)).toBe(false);
    expect(matchesMetadata(filter, 10, null, null)).toBe(true);
    expect(matchesMetadata(filter, 20, null, null)).toBe(true);
    expect(matchesMetadata(filter, 21, null, null)).toBe(false);
    expect(matchesMetadata(compiled({ min_size: 5, max_size: 5 }), 5, null, null)).toBe(true);
    const reversed = compileFilter(spec({ min_size: 21, max_size: 20 }));
    expect(reversed).toEqual({ ok: false, message: "min_size (21) is larger than max_size (20)" });
    expect(compileFilter(spec({ min_size: -1 })).ok).toBe(false);
    expect(compileFilter(spec({ max_size: 1.5 })).ok).toBe(false);
  });

  it("makes the date interval after-inclusive and before-exclusive", () => {
    const filter = compiled({ modified_after: iso(2024, 1, 1), modified_before: iso(2025, 1, 1) });
    expect(matchesMetadata(filter, 0, ts(2024, 1, 1) - 1, null)).toBe(false);
    expect(matchesMetadata(filter, 0, ts(2024, 1, 1), null)).toBe(true);
    expect(matchesMetadata(filter, 0, ts(2024, 12, 31), null)).toBe(true);
    expect(matchesMetadata(filter, 0, ts(2025, 1, 1), null)).toBe(false);
    // An unknown modification time never satisfies a date bound.
    expect(matchesMetadata(filter, 0, null, null)).toBe(false);

    const onlyAfter = compiled({ modified_after: iso(2024, 1, 1) });
    expect(matchesMetadata(onlyAfter, 0, ts(2030, 1, 1), null)).toBe(true);
    expect(matchesMetadata(onlyAfter, 0, null, null)).toBe(false);

    for (const [after, before] of [
      [iso(2025, 1, 1), iso(2024, 1, 1)],
      [iso(2024, 1, 1), iso(2024, 1, 1)],
    ] as const) {
      const result = compileFilter(spec({ modified_after: after, modified_before: before }));
      expect(result).toEqual({
        ok: false,
        message: "modified_after must be earlier than modified_before (the interval is empty)",
      });
    }
    expect(compileFilter(spec({ modified_after: "yesterday" }))).toEqual({
      ok: false,
      message: "modified_after is not a valid RFC 3339 time",
    });
  });

  it("defaults storage classes to STANDARD and ignores case", () => {
    const glacier = compiled({ storage_classes: [" glacier ", "DEEP_ARCHIVE"] });
    expect(matchesMetadata(glacier, 0, null, "GLACIER")).toBe(true);
    expect(matchesMetadata(glacier, 0, null, "deep_archive")).toBe(true);
    expect(matchesMetadata(glacier, 0, null, "STANDARD")).toBe(false);
    expect(matchesMetadata(glacier, 0, null, null)).toBe(false);

    const standard = compiled({ storage_classes: ["standard"] });
    expect(matchesMetadata(standard, 0, null, null)).toBe(true);
    expect(matchesMetadata(standard, 0, null, "")).toBe(true);

    expect(matchesMetadata(compiled({}), 0, null, "ANYTHING")).toBe(true);
    expect(compileFilter(spec({ storage_classes: ["  "] }))).toEqual({
      ok: false,
      message: "storage_classes contains an empty name",
    });
  });

  it("matches objects relative to the listing prefix", () => {
    const filter = compiled({ prefix: "2024/", glob: "**/*.jpg", min_size: 1 });
    expect(matchesObject(filter, object("photos/2024/a.jpg", 5, null, null), "photos/")).toBe(true);
    expect(matchesObject(filter, object("photos/2024/a.jpg", 0, null, null), "photos/")).toBe(false);
    expect(matchesObject(filter, object("photos/2023/a.jpg", 5, null, null), "photos/")).toBe(false);
    expect(matchesObject(filter, object("other/2024/a.jpg", 5, null, null), "photos/")).toBe(false);
    // Without a listing prefix the full key is relative.
    expect(matchesObject(filter, object("photos/2024/a.jpg", 5, null, null), "")).toBe(false);
    expect(matchesObject(filter, object("2024/a.jpg", 5, null, null), "")).toBe(true);
  });

  it("matches folder entries by the prefix criterion only", () => {
    const filter = compiled({ prefix: "2024/", glob: "*.jpg", modified_after: iso(2024, 1, 1) });
    expect(matchesObject(filter, folder("photos/2024/"), "photos/")).toBe(true);
    expect(matchesObject(filter, folder("photos/2023/"), "photos/")).toBe(false);
    // A zero-byte folder marker is a folder as well.
    expect(matchesObject(filter, object("photos/2024/06/", 0, null, null), "photos/")).toBe(true);
  });

  it("evaluates every criterion together on a loaded entry", () => {
    const filter = compiled({
      glob: "*.log",
      min_size: 100,
      modified_after: iso(2026, 10, 1),
      storage_classes: ["STANDARD_IA"],
    });
    const modified = "2026-10-15T12:00:00.123456789Z";
    expect(parseTimestamp(modified)).not.toBeNull();
    expect(matchesObject(filter, object("a.log", 100, modified, "STANDARD_IA"), "")).toBe(true);
    expect(matchesObject(filter, object("a.log", 99, modified, "STANDARD_IA"), "")).toBe(false);
    expect(matchesObject(filter, object("a.log", 100, modified, null), "")).toBe(false);
    expect(matchesObject(filter, object("a.txt", 100, modified, "STANDARD_IA"), "")).toBe(false);
    expect(matchesObject(filter, object("a.log", 100, "2026-09-30T23:59:59Z", "STANDARD_IA"), "")).toBe(false);
  });
});
