import { describe, expect, it } from "vitest";
import {
  generateCustomerKey,
  headerChange,
  isArchiveClass,
  metaBytes,
  metaPatch,
  normalizeCustomerKey,
  recordToRows,
  validateCustomerKey,
  validateHeaderValue,
  validateMediaType,
  validateMetaKey,
  validateMetaRows,
  validateTag,
  validateTagRows,
} from "./objectMeta";

describe("tags", () => {
  it("accepts what S3 accepts and explains what it does not", () => {
    for (const [key, value] of [
      ["env", "prod"],
      ["team name", "data eng"],
      ["cost-center", "a+b=c.d_e:f/g@h"],
      ["ünï", "valeur"],
      ["empty", ""],
    ] as const) {
      expect(validateTag(key, value), `${key}=${value}`).toBeNull();
    }
    expect(validateTag("", "v")).toMatch(/cannot be empty/);
    expect(validateTag("aws:stack", "v")).toMatch(/aws:/);
    expect(validateTag("AWS:Stack", "v")).toMatch(/aws:/);
    expect(validateTag("bad,key", "v")).toMatch(/letters, digits/);
    expect(validateTag("k", "semi;colon")).toMatch(/tag value/);
    expect(validateTag("k".repeat(129), "v")).toMatch(/128/);
    expect(validateTag("k", "v".repeat(257))).toMatch(/256/);
    expect(validateTag("k".repeat(128), "v".repeat(256))).toBeNull();
  });

  it("checks lists: duplicates and the limit of ten", () => {
    const ten = Array.from({ length: 10 }, (_, index) => ({ key: `k${index}`, value: "v" }));
    expect(validateTagRows(ten)).toBeNull();
    expect(validateTagRows([...ten, { key: "k10", value: "v" }])).toMatch(/at most 10/);
    expect(validateTagRows([{ key: "a", value: "1" }, { key: "a", value: "2" }])).toMatch(/twice/);
    expect(validateTagRows([{ key: "a", value: "no;" }])).not.toBeNull();
  });
});

describe("headers and user metadata", () => {
  it("allows printable ASCII only", () => {
    expect(validateHeaderValue("Cache-Control", "public, max-age=3600")).toBeNull();
    expect(validateHeaderValue("Cache-Control", "")).toMatch(/empty/);
    expect(validateHeaderValue("Cache-Control", "line\nbreak")).toMatch(/ASCII/);
    expect(validateHeaderValue("Cache-Control", "café")).toMatch(/ASCII/);
    expect(validateHeaderValue("Cache-Control", "a".repeat(2049))).toMatch(/2048/);
  });

  it("checks media types and metadata keys", () => {
    for (const good of ["text/plain", "application/json; charset=utf-8", "image/svg+xml"]) {
      expect(validateMediaType(good), good).toBeNull();
    }
    for (const bad of ["text", "/plain", "text/", "te xt/plain", "a/b/c"]) {
      expect(validateMediaType(bad), bad).not.toBeNull();
    }
    expect(validateMetaKey("Cost-Center")).toBeNull();
    expect(validateMetaKey("two words")).not.toBeNull();
    expect(validateMetaKey("")).not.toBeNull();
    expect(validateMetaKey("ünï")).not.toBeNull();
  });

  it("limits the user metadata to 2048 bytes of keys and values", () => {
    expect(metaBytes({ ab: "cde" })).toBe(5);
    expect(metaBytes({ k: "é" })).toBe(3);
    expect(validateMetaRows([{ key: "a", value: "x".repeat(1000) }, { key: "b", value: "y".repeat(1046) }])).toBeNull();
    expect(validateMetaRows([{ key: "a", value: "x".repeat(1000) }, { key: "b", value: "y".repeat(1047) }])).toMatch(/2049 bytes/);
    expect(validateMetaRows([{ key: "Owner", value: "a" }, { key: "owner", value: "b" }])).toMatch(/twice/);
    expect(validateMetaRows([{ key: "bad key", value: "a" }])).not.toBeNull();
  });
});

describe("patches", () => {
  it("builds the header patch of a single object from what the field started with", () => {
    expect(headerChange("text/plain", "text/plain", false, false)).toEqual({ action: "keep" });
    expect(headerChange("text/plain", "text/html", false, false)).toEqual({ action: "set", value: "text/html" });
    expect(headerChange("text/plain", "  ", false, false)).toEqual({ action: "remove" });
    expect(headerChange("", "", false, false)).toEqual({ action: "keep" });
    expect(headerChange("", "no-store", false, false)).toEqual({ action: "set", value: "no-store" });
  });

  it("builds the header patch of a batch: empty keeps, a value sets, the checkbox removes", () => {
    expect(headerChange("", "", false, true)).toEqual({ action: "keep" });
    expect(headerChange("", "no-store", false, true)).toEqual({ action: "set", value: "no-store" });
    expect(headerChange("", "no-store", true, true)).toEqual({ action: "remove" });
  });

  it("diffs user metadata rows against what the object has", () => {
    const initial = { owner: "team", stage: "raw" };
    const rows = [
      { key: "Stage", value: "clean" },
      { key: "new", value: "1" },
    ];
    expect(metaPatch(initial, rows)).toEqual({ set: { stage: "clean", new: "1" }, remove: ["owner"] });
    expect(metaPatch(initial, recordToRows(initial))).toEqual({ set: {}, remove: [] });
  });

  it("sorts rows by key", () => {
    expect(recordToRows({ b: "2", a: "1" }).map((row) => row.key)).toEqual(["a", "b"]);
  });
});

describe("customer keys", () => {
  const HEX = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
  const BASE64 = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

  it("reads hex and base64 and normalises both to hex", () => {
    expect(normalizeCustomerKey(HEX)).toBe(HEX);
    expect(normalizeCustomerKey(HEX.toUpperCase())).toBe(HEX);
    expect(normalizeCustomerKey(BASE64)).toBe(HEX);
    expect(normalizeCustomerKey(BASE64.replace("=", ""))).toBe(HEX);
    expect(normalizeCustomerKey(`  ${BASE64}\n`)).toBe(HEX);
  });

  it("rejects anything else with a message that does not repeat the input", () => {
    for (const bad of ["", "secret", "0123456789abcdef", HEX.slice(1), "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHg=="]) {
      expect(normalizeCustomerKey(bad), bad).toBeNull();
    }
    expect(validateCustomerKey("hunter2")).not.toContain("hunter2");
    expect(validateCustomerKey("")).toMatch(/Enter/);
    expect(validateCustomerKey(BASE64)).toBeNull();
  });

  it("generates a valid random key each time", () => {
    const first = generateCustomerKey();
    expect(normalizeCustomerKey(first)).not.toBeNull();
    expect(generateCustomerKey()).not.toBe(first);
  });
});

describe("classes", () => {
  it("knows which classes are archives", () => {
    expect(isArchiveClass("GLACIER")).toBe(true);
    expect(isArchiveClass("DEEP_ARCHIVE")).toBe(true);
    expect(isArchiveClass("GLACIER_IR")).toBe(false);
    expect(isArchiveClass(null)).toBe(false);
  });
});
