import { describe, expect, it } from "vitest";
import {
  baseName,
  breadcrumbs,
  destinationKey,
  displaySegment,
  EMPTY_SEGMENT_LABEL,
  localBaseName,
  normalizePrefixInput,
  parentPrefix,
  pasteProblem,
  relativeName,
  validateNewName,
} from "./s3keys";

describe("key helpers", () => {
  it("finds parents and base names of objects and prefixes", () => {
    expect(parentPrefix("a/b/c.txt")).toBe("a/b/");
    expect(parentPrefix("a/b/")).toBe("a/");
    expect(parentPrefix("top.txt")).toBe("");
    expect(parentPrefix("a//")).toBe("a/");
    expect(parentPrefix("//")).toBe("/");
    expect(baseName("a/b/")).toBe("b");
    expect(baseName("a/b/c d.txt")).toBe("c d.txt");
    expect(baseName("a//")).toBe("");
  });

  it("names entries relative to the listing prefix (flat listings keep inner paths)", () => {
    expect(relativeName("photos/2024/", "photos/")).toBe("2024");
    expect(relativeName("photos/2024/a.jpg", "photos/")).toBe("2024/a.jpg");
    expect(relativeName("odd//", "odd/")).toBe("");
    expect(displaySegment("")).toBe(EMPTY_SEGMENT_LABEL);
    expect(displaySegment("café")).toBe("café");
  });

  it("builds breadcrumbs including empty segments and unicode", () => {
    expect(breadcrumbs("")).toEqual([]);
    expect(breadcrumbs("a//b/")).toEqual([
      { name: "a", prefix: "a/" },
      { name: "", prefix: "a//" },
      { name: "b", prefix: "a//b/" },
    ]);
    expect(breadcrumbs("emoji \u{1F4F7}/x y/")).toEqual([
      { name: "emoji \u{1F4F7}", prefix: "emoji \u{1F4F7}/" },
      { name: "x y", prefix: "emoji \u{1F4F7}/x y/" },
    ]);
  });

  it("applies the folder semantics of the contract to destinations", () => {
    expect(destinationKey("photos/2024/", "backup/")).toBe("backup/2024/");
    expect(destinationKey("photos/2024/a.jpg", "")).toBe("a.jpg");
  });

  it("validates new names", () => {
    const existing = new Set(["docs/", "readme.md"]);
    expect(validateNewName("", { prefix: "", folder: true })).toMatch(/name/);
    expect(validateNewName("   ", { prefix: "", folder: true })).toMatch(/spaces/);
    expect(validateNewName("a/b", { prefix: "", folder: true })).toMatch(/slash/);
    expect(validateNewName("..", { prefix: "", folder: false })).toMatch(/reserved/);
    expect(validateNewName("docs", { prefix: "", folder: true, existingKeys: existing })).toMatch(/already exists/);
    expect(validateNewName("readme.md", { prefix: "", folder: false, existingKeys: existing })).toMatch(/already/);
    expect(validateNewName("x".repeat(1025), { prefix: "", folder: false })).toMatch(/1024 bytes/);
    expect(validateNewName("new folder", { prefix: "a/", folder: true, existingKeys: existing })).toBeNull();
  });

  it("refuses pastes into the same folder or into the copied folder itself", () => {
    expect(pasteProblem("b", ["a/x.txt"], "b", "a/")).toMatch(/already in this folder/);
    expect(pasteProblem("b", ["a/"], "b", "a/sub/")).toMatch(/into itself/);
    expect(pasteProblem("b", ["a/"], "b", "a/")).toMatch(/into itself/);
    expect(pasteProblem("b", ["a/x.txt"], "other", "a/")).toBeNull();
    expect(pasteProblem("b", ["a/x.txt"], "b", "c/")).toBeNull();
    expect(pasteProblem("b", [], "b", "c/")).toMatch(/empty/);
  });

  it("normalises typed prefixes and local names", () => {
    expect(normalizePrefixInput("")).toBe("");
    expect(normalizePrefixInput("logs")).toBe("logs/");
    expect(normalizePrefixInput("logs/")).toBe("logs/");
    expect(localBaseName("/home/demo/file.txt")).toBe("file.txt");
    expect(localBaseName("C:\\Users\\demo\\dir\\")).toBe("dir");
  });
});
