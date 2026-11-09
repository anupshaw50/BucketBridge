import { describe, expect, it } from "vitest";
import { conflicts, hasTranslation } from "./index";
import { en } from "./en";
import { zhMenu } from "./zh/menu";

/** Every source file of the interface, read at build time as plain text. */
const SOURCES: Record<string, string> = import.meta.glob(["../**/*.ts", "../**/*.tsx"], {
  query: "?raw",
  import: "default",
  eager: true,
});

/** Directories whose text is not part of the localised interface. */
const SKIP = new Set(["i18n", "mock", "test", "e2e"]);

const LITERAL = /\btr\(\s*(['"])((?:\\.|(?!\1)[^\\])*)\1/g;

function unescape(literal: string): string {
  return literal.replace(/\\(.)/g, (_match, char: string) => {
    switch (char) {
      case "n":
        return "\n";
      case "r":
        return "\r";
      case "t":
        return "\t";
      default:
        return char;
    }
  });
}

/** Every English sentence handed to `tr` in the sources, with the file that uses it. */
function wrapped(): { file: string; source: string }[] {
  const seen: { file: string; source: string }[] = [];
  for (const [path, text] of Object.entries(SOURCES)) {
    const parts = segments(path);
    if (parts.some((part) => SKIP.has(part))) continue;
    const file = parts.join("/");
    for (const match of text.matchAll(LITERAL)) {
      const source = match[2];
      if (source !== undefined) seen.push({ file, source: unescape(source) });
    }
  }
  return seen;
}

/** Glob keys are relative to this file (src/i18n), so `..` walks back to src/. */
function segments(relative: string): string[] {
  const parts = ["src", "i18n"];
  for (const part of relative.split("/")) {
    if (part === "..") parts.pop();
    else if (part !== "." && part !== "") parts.push(part);
  }
  return parts;
}

describe("translation coverage", () => {
  it("has a Chinese text for every sentence passed to tr", () => {
    const missing = wrapped()
      .filter(({ source }) => !hasTranslation(source))
      .map(({ file, source }) => `${file}: ${source}`);
    expect(missing).toEqual([]);
  });

  it("has no sentence translated twice in different ways", () => {
    expect(conflicts).toEqual([]);
  });

  it("keeps a label for every menu entry, with no empty text and no em dash", () => {
    for (const text of [...Object.values(en), ...Object.values(zhMenu)]) {
      expect(text.trim()).not.toBe("");
      expect(text).not.toContain(String.fromCharCode(0x2014));
    }
    for (const [key, text] of Object.entries(zhMenu)) {
      expect(text.trim(), key).not.toBe("");
    }
  });

  it("translates the menu labels rather than copying the English bundle", () => {
    const english = new Map(Object.entries(en));
    const unchanged = Object.entries(zhMenu)
      .filter(([key, text]) => english.get(key as keyof typeof en) === text)
      .map(([key]) => key);
    // Only proper nouns (CloudFront, CORS, …) may read the same in both languages.
    expect(unchanged.length).toBeLessThanOrEqual(3);
  });
});
