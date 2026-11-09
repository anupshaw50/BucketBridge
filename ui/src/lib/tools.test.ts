import { describe, expect, it } from "vitest";
import type { UploadRow } from "../api/types";
import { tr } from "../i18n";
import { describeFilter, emptyFilterSpec } from "./filters";
import {
  dateToTimestamp,
  exportFileName,
  formatAge,
  joinBytes,
  splitBytes,
  timestampToDate,
  uploadAgeDays,
  uploadId,
  uploadsOlderThan,
} from "./tools";

const NOW = Date.UTC(2026, 9, 29, 12, 0, 0);

function row(key: string, initiated: string | null): UploadRow {
  return { bucket: "b", key, upload_id: `id-${key}`, initiated, storage_class: null, initiator: null };
}

function daysAgo(days: number): string {
  return new Date(NOW - days * 86_400_000).toISOString();
}

describe("upload ages", () => {
  it("counts whole days and never goes negative", () => {
    expect(uploadAgeDays(daysAgo(0), NOW)).toBe(0);
    expect(uploadAgeDays(daysAgo(1.5), NOW)).toBe(1);
    expect(uploadAgeDays(daysAgo(45), NOW)).toBe(45);
    expect(uploadAgeDays(new Date(NOW + 5000).toISOString(), NOW)).toBe(0);
    expect(uploadAgeDays(null, NOW)).toBeNull();
    expect(uploadAgeDays("not a date", NOW)).toBeNull();
    expect(formatAge(daysAgo(0), NOW)).toBe(tr("today"));
    expect(formatAge(daysAgo(1), NOW)).toBe(tr("1 day"));
    expect(formatAge(daysAgo(45), NOW)).toBe(tr("{n} days", { n: 45 }));
    expect(formatAge(null, NOW)).toBe("");
  });

  it("selects the uploads older than N days and skips undated ones", () => {
    const rows = [row("new", daysAgo(2)), row("edge", daysAgo(30)), row("old", daysAgo(31)), row("older", daysAgo(200)), row("undated", null)];
    expect(uploadsOlderThan(rows, 30, NOW).map((entry) => entry.key)).toEqual(["old", "older"]);
    expect(uploadsOlderThan(rows, 0, NOW).map((entry) => entry.key)).toEqual(["new", "edge", "old", "older"]);
    expect(uploadsOlderThan(rows, 365, NOW)).toEqual([]);
  });

  it("identifies an upload by bucket, key and upload id", () => {
    expect(uploadId({ bucket: "b", key: "k", upload_id: "u" })).not.toBe(uploadId({ bucket: "b", key: "k", upload_id: "v" }));
    expect(uploadId({ bucket: "a", key: "b/c", upload_id: "u" })).not.toBe(uploadId({ bucket: "a/b", key: "c", upload_id: "u" }));
  });
});

describe("export file names", () => {
  it("builds a safe name from the bucket and folder", () => {
    expect(exportFileName("demo-photos", "", "csv")).toBe("demo-photos.csv");
    expect(exportFileName("demo-photos", "photos/2024/", "xlsx")).toBe("demo-photos-photos_2024.xlsx");
    expect(exportFileName("b", "weird name/\u00e9\u00e8/", "parquet")).toBe("b-weird_name.parquet");
    expect(exportFileName("b", "////", "csv")).toBe("b.csv");
    expect(exportFileName("b", "x".repeat(200), "csv").length).toBeLessThanOrEqual(2 + 1 + 60 + 4);
  });
});

describe("size units", () => {
  it("splits into the largest even unit and joins back", () => {
    expect(splitBytes(0)).toEqual({ value: 0, unit: "B" });
    expect(splitBytes(1500)).toEqual({ value: 1500, unit: "B" });
    expect(splitBytes(1024)).toEqual({ value: 1, unit: "KiB" });
    expect(splitBytes(3 * 1024 ** 2)).toEqual({ value: 3, unit: "MiB" });
    expect(splitBytes(1024 ** 3 + 1)).toEqual({ value: 1024 ** 3 + 1, unit: "B" });
    expect(splitBytes(2 * 1024 ** 4)).toEqual({ value: 2, unit: "TiB" });
    expect(joinBytes(3, "MiB")).toBe(3 * 1024 ** 2);
    expect(joinBytes("1.5", "KiB")).toBe(1536);
    expect(joinBytes("", "B")).toBeNull();
    expect(joinBytes(-1, "B")).toBeNull();
    expect(joinBytes("abc", "B")).toBeNull();
  });
});

describe("date inputs", () => {
  it("maps a day to UTC midnight and back", () => {
    expect(dateToTimestamp("2026-10-29")).toBe("2026-10-29T00:00:00Z");
    expect(dateToTimestamp("")).toBeNull();
    expect(dateToTimestamp("29.10.2026")).toBeNull();
    expect(timestampToDate("2026-10-29T00:00:00Z")).toBe("2026-10-29");
    expect(timestampToDate("2026-10-29T23:30:00Z")).toBe("2026-10-29");
    expect(timestampToDate(null)).toBe("");
    expect(timestampToDate("garbage")).toBe("");
  });
});

describe("describeFilter", () => {
  it("lists the criteria of a filter", () => {
    expect(describeFilter(emptyFilterSpec())).toBe(tr("Matches everything"));
    expect(
      describeFilter({
        prefix: "logs/",
        glob: "**/*.gz",
        min_size: 1024,
        max_size: 1024 ** 2,
        modified_after: "2026-01-01T00:00:00Z",
        modified_before: "2026-07-01T00:00:00Z",
        storage_classes: ["STANDARD_IA", "GLACIER"],
      }),
    ).toBe(
      [
        tr("prefix \"{prefix}\"", { prefix: "logs/" }),
        tr("glob {glob}", { glob: "**/*.gz" }),
        tr("size {min} to {max}", { min: "1.0 KiB", max: "1.0 MiB" }),
        tr("modified on or after {time}", { time: "2026-01-01" }),
        tr("modified before {time}", { time: "2026-07-01" }),
        tr("class {classes}", { classes: "STANDARD_IA, GLACIER" }),
      ].join("; "),
    );
    expect(describeFilter({ ...emptyFilterSpec(), min_size: 10 })).toBe(tr("size at least {min}", { min: "10 B" }));
    expect(describeFilter({ ...emptyFilterSpec(), max_size: 2048 })).toBe(tr("size at most {max}", { max: "2.0 KiB" }));
  });
});
