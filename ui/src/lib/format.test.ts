import { describe, expect, it } from "vitest";
import { formatBytes, formatCount, formatDateTime, formatEta, formatSpeed, parseTimestamp, percent, plural } from "./format";

describe("formatBytes", () => {
  it("uses binary units with up to three significant digits", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(1023)).toBe("1023 B");
    expect(formatBytes(1024)).toBe("1.0 KiB");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MiB");
    expect(formatBytes(150 * 1024 * 1024)).toBe("150 MiB");
    expect(formatBytes(5 * 1024 ** 4)).toBe("5.0 TiB");
  });

  it("never prints 1024 of a unit after rounding", () => {
    expect(formatBytes(1024 * 1024 - 1)).toBe("1.0 MiB");
  });

  it("returns an empty string for unknown or invalid sizes", () => {
    expect(formatBytes(null)).toBe("");
    expect(formatBytes(undefined)).toBe("");
    expect(formatBytes(-1)).toBe("");
    expect(formatBytes(Number.NaN)).toBe("");
  });
});

describe("formatSpeed and formatEta", () => {
  it("formats speeds and hides idle ones", () => {
    expect(formatSpeed(2 * 1024 * 1024)).toBe("2.0 MiB/s");
    expect(formatSpeed(0)).toBe("");
    expect(formatSpeed(null)).toBe("");
  });

  it("formats remaining time compactly", () => {
    expect(formatEta(0)).toBe("0s");
    expect(formatEta(44.2)).toBe("45s");
    expect(formatEta(185)).toBe("3m 05s");
    expect(formatEta(2 * 3600 + 3 * 60)).toBe("2h 03m");
    expect(formatEta(3 * 86_400 + 4 * 3600)).toBe("3d 04h");
    expect(formatEta(null)).toBe("");
    expect(formatEta(Number.POSITIVE_INFINITY)).toBe("");
  });
});

describe("timestamps", () => {
  it("parses RFC 3339 with any precision and offsets", () => {
    expect(parseTimestamp("2026-10-29T08:15:00Z")).toBe(Date.UTC(2026, 9, 29, 8, 15, 0));
    expect(parseTimestamp("2026-10-29T08:15:00.123456789Z")).toBe(Date.UTC(2026, 9, 29, 8, 15, 0, 123));
    expect(parseTimestamp("2026-10-29T10:15:00+02:00")).toBe(Date.UTC(2026, 9, 29, 8, 15, 0));
    expect(parseTimestamp("2026-10-29T03:15:00-05:00")).toBe(Date.UTC(2026, 9, 29, 8, 15, 0));
  });

  it("rejects malformed and impossible dates", () => {
    expect(parseTimestamp("yesterday")).toBeNull();
    expect(parseTimestamp("2026-02-30T00:00:00Z")).toBeNull();
    expect(parseTimestamp("2026-13-01T00:00:00Z")).toBeNull();
    expect(parseTimestamp(null)).toBeNull();
  });

  it("formats in local time (UTC in tests) and keeps unparseable text", () => {
    expect(formatDateTime("2026-10-29T08:15:09.5Z")).toBe("2026-10-29 08:15:09");
    expect(formatDateTime("not a date")).toBe("not a date");
    expect(formatDateTime(null)).toBe("");
  });
});

describe("counts", () => {
  it("formats counts, plurals and percentages", () => {
    expect(formatCount(10_000)).toBe("10,000");
    expect(plural(1, "item")).toBe("1 item");
    expect(plural(2, "item")).toBe("2 items");
    expect(plural(1_500, "entry", "entries")).toBe("1,500 entries");
    expect(percent(5, 10)).toBe(50);
    expect(percent(15, 10)).toBe(100);
    expect(percent(0, 0)).toBeNull();
    expect(percent(1, null)).toBeNull();
  });
});
