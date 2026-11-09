import type { ListingFormat, UploadRow } from "../api/types";
import { tr } from "../i18n";
import { parseTimestamp } from "./format";

const DAY_MS = 86_400_000;

/** Whole days between an upload's start and `now`, or `null` when the server sent no start time. */
export function uploadAgeDays(initiated: string | null, now: number): number | null {
  const started = parseTimestamp(initiated);
  if (started === null) {
    return null;
  }
  return Math.max(0, Math.floor((now - started) / DAY_MS));
}

/** "today", "1 day", "45 days" for the age column; empty when unknown. */
export function formatAge(initiated: string | null, now: number): string {
  const days = uploadAgeDays(initiated, now);
  if (days === null) {
    return "";
  }
  if (days === 0) {
    return tr("today");
  }
  return days === 1 ? tr("1 day") : tr("{n} days", { n: days });
}

/** Stable identity of an upload across listings. */
export function uploadId(row: { bucket: string; key: string; upload_id: string }): string {
  return JSON.stringify([row.bucket, row.key, row.upload_id]);
}

/**
 * The uploads that were started more than `days` days before `now`. Uploads without a start time are
 * never selected: nothing says how old they are.
 */
export function uploadsOlderThan(rows: readonly UploadRow[], days: number, now: number): UploadRow[] {
  return rows.filter((row) => {
    const started = parseTimestamp(row.initiated);
    return started !== null && now - started > days * DAY_MS;
  });
}

export const FORMAT_LABELS: Record<ListingFormat, string> = {
  csv: "CSV",
  xlsx: tr("Excel workbook"),
  parquet: "Parquet",
};

export const FORMAT_EXTENSIONS: Record<ListingFormat, string> = {
  csv: "csv",
  xlsx: "xlsx",
  parquet: "parquet",
};

/** A file name for an export of `s3://bucket/prefix`: `demo-photos-photos_2024.csv`. */
export function exportFileName(bucket: string, prefix: string, format: ListingFormat): string {
  const folder = prefix
    .replace(/\/+$/, "")
    .replace(/[^A-Za-z0-9._-]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 60);
  const stem = folder.length > 0 ? `${bucket}-${folder}` : bucket;
  return `${stem}.${FORMAT_EXTENSIONS[format]}`;
}

export type SizeUnit = "B" | "KiB" | "MiB" | "GiB" | "TiB";

export const SIZE_UNITS: readonly { unit: SizeUnit; factor: number }[] = [
  { unit: "B", factor: 1 },
  { unit: "KiB", factor: 1024 },
  { unit: "MiB", factor: 1024 ** 2 },
  { unit: "GiB", factor: 1024 ** 3 },
  { unit: "TiB", factor: 1024 ** 4 },
];

/** The largest unit that divides `bytes` evenly: 1048576 is 1 MiB, 1500 is 1500 B. */
export function splitBytes(bytes: number): { value: number; unit: SizeUnit } {
  for (const { unit, factor } of [...SIZE_UNITS].reverse()) {
    if (bytes >= factor && bytes % factor === 0) {
      return { value: bytes / factor, unit };
    }
  }
  return { value: bytes, unit: "B" };
}

/** `value` in `unit` as a whole number of bytes, or `null` when it is not a usable number. */
export function joinBytes(value: number | string, unit: SizeUnit): number | null {
  if (value === "") {
    return null;
  }
  const number = typeof value === "number" ? value : Number(value);
  const factor = SIZE_UNITS.find((entry) => entry.unit === unit)?.factor ?? 1;
  const bytes = Math.round(number * factor);
  return Number.isSafeInteger(bytes) && bytes >= 0 ? bytes : null;
}

/** A date input value (`2026-10-29`) as the UTC midnight the filters compare against; `null` for empty input. */
export function dateToTimestamp(date: string): string | null {
  return /^\d{4}-\d{2}-\d{2}$/.test(date) ? `${date}T00:00:00Z` : null;
}

/** The date input value for a stored timestamp (the day it falls on in UTC), or an empty string. */
export function timestampToDate(timestamp: string | null): string {
  const ms = parseTimestamp(timestamp);
  return ms === null ? "" : new Date(ms).toISOString().slice(0, 10);
}
