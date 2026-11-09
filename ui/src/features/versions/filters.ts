import type { DeleteMarkerMode, VersionFilter } from "../../api/types";
import { tr } from "../../i18n";
import { formatDateTime } from "../../lib/format";
import { baseName } from "../../lib/s3keys";

/** What the filter panel edits; `toVersionFilter` turns it into the contract's filter. */
export interface VersionFilterForm {
  showVersions: boolean;
  deleteMarkers: DeleteMarkerMode;
  multipleVersions: boolean;
  moreThan: number | "";
  fewerThan: number | "";
  /** Local dates as YYYY-MM-DD; both ends are included. */
  from: string;
  to: string;
}

export const DEFAULT_FORM: VersionFilterForm = {
  showVersions: true,
  deleteMarkers: "include",
  multipleVersions: false,
  moreThan: "",
  fewerThan: "",
  from: "",
  to: "",
};

export const DEFAULT_FILTER: VersionFilter = {
  show_versions: true,
  delete_markers: "include",
  multiple_versions: false,
  count_greater_than: null,
  count_less_than: null,
  modified_after: null,
  modified_before: null,
};

export type FilterResult = { filter: VersionFilter; error: null } | { filter: null; error: string };

const DAY_MS = 86_400_000;

/** The start of a UTC day as epoch milliseconds; null for anything but a real YYYY-MM-DD date. */
function dayStart(text: string): number | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(text);
  if (!match) {
    return null;
  }
  const [year, month, day] = [Number(match[1]), Number(match[2]), Number(match[3])];
  const time = Date.UTC(year, month - 1, day);
  const check = new Date(time);
  return check.getUTCFullYear() === year && check.getUTCMonth() === month - 1 && check.getUTCDate() === day ? time : null;
}

function iso(ms: number): string {
  return new Date(ms).toISOString().replace(".000Z", "Z");
}

function count(value: number | "", label: string): { value: number | null; error: string | null } {
  if (value === "") {
    return { value: null, error: null };
  }
  if (!Number.isInteger(value) || value < 0) {
    return { value: null, error: tr("{label} must be a whole number, 0 or more.", { label }) };
  }
  return { value, error: null };
}

/**
 * Checks the form with the rules of the backend (nothing to show, an empty date
 * interval, counts that no key can satisfy) and builds the filter. Dates are UTC days.
 */
export function toVersionFilter(form: VersionFilterForm): FilterResult {
  const fail = (error: string): FilterResult => ({ filter: null, error });
  if (!form.showVersions && form.deleteMarkers === "exclude") {
    return fail(tr("Nothing would be listed: show versions or delete markers."));
  }
  const from = form.from === "" ? null : dayStart(form.from);
  const to = form.to === "" ? null : dayStart(form.to);
  if ((form.from !== "" && from === null) || (form.to !== "" && to === null)) {
    return fail(tr("Enter dates as YYYY-MM-DD."));
  }
  if (from !== null && to !== null && to < from) {
    return fail(tr("The end date is before the start date."));
  }
  const more = count(form.moreThan, tr("More than"));
  const fewer = count(form.fewerThan, tr("Fewer than"));
  const countError = more.error ?? fewer.error;
  if (countError !== null) {
    return fail(countError);
  }
  const least = Math.max((more.value ?? 0) + 1, form.multipleVersions ? 2 : 1);
  if (fewer.value !== null && fewer.value <= least) {
    return fail(tr("No key can have at least {least} and fewer than {fewer} versions.", { least, fewer: fewer.value }));
  }
  return {
    filter: {
      show_versions: form.showVersions,
      delete_markers: form.deleteMarkers,
      multiple_versions: form.multipleVersions,
      count_greater_than: more.value,
      count_less_than: fewer.value,
      modified_after: from === null ? null : iso(from),
      modified_before: to === null ? null : iso(to + DAY_MS),
    },
    error: null,
  };
}

/** `report (2026-10-14 09-30-00).pdf`: the name a downloaded version is offered under. */
export function versionFileName(key: string, modified: string | null): string {
  const name = baseName(key);
  const stamp = formatDateTime(modified).replace(" ", "_").replaceAll(":", "-");
  const dot = name.lastIndexOf(".");
  const suffix = stamp === "" ? "version" : stamp;
  return dot > 0 ? `${name.slice(0, dot)} (${suffix})${name.slice(dot)}` : `${name} (${suffix})`;
}

/** Key of a version in a selection. */
export function versionId(key: string, version: string): string {
  return `${key}\u0000${version}`;
}
