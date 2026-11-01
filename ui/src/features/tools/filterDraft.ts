import type { FilterSpec, SavedFilter } from "../../api/types";
import { emptyFilterSpec } from "../../lib/filters";
import { dateToTimestamp, joinBytes, type SizeUnit, splitBytes, timestampToDate } from "../../lib/tools";

/** The storage classes S3 reports, offered by the filter form. */
export const STORAGE_CLASS_OPTIONS: readonly string[] = [
  "STANDARD",
  "STANDARD_IA",
  "ONEZONE_IA",
  "INTELLIGENT_TIERING",
  "GLACIER_IR",
  "GLACIER",
  "DEEP_ARCHIVE",
  "REDUCED_REDUNDANCY",
];

/** Form state of the saved-filter editor. */
export interface FilterDraft {
  name: string;
  prefix: string;
  glob: string;
  minSize: number | string;
  minUnit: SizeUnit;
  maxSize: number | string;
  maxUnit: SizeUnit;
  /** A date input value, `YYYY-MM-DD` or empty. */
  after: string;
  before: string;
  classes: string[];
}

export function emptyDraft(): FilterDraft {
  return {
    name: "",
    prefix: "",
    glob: "",
    minSize: "",
    minUnit: "MiB",
    maxSize: "",
    maxUnit: "MiB",
    after: "",
    before: "",
    classes: [],
  };
}

export function draftFromSaved(saved: SavedFilter): FilterDraft {
  const spec = saved.filter;
  const min = spec.min_size === null ? null : splitBytes(spec.min_size);
  const max = spec.max_size === null ? null : splitBytes(spec.max_size);
  return {
    name: saved.name,
    prefix: spec.prefix ?? "",
    glob: spec.glob ?? "",
    minSize: min?.value ?? "",
    minUnit: min?.unit ?? "MiB",
    maxSize: max?.value ?? "",
    maxUnit: max?.unit ?? "MiB",
    after: timestampToDate(spec.modified_after),
    before: timestampToDate(spec.modified_before),
    classes: [...spec.storage_classes],
  };
}

function blankToNull(value: string): string | null {
  return value.length === 0 ? null : value;
}

/** The spec a draft stands for; blank fields are unset. */
export function specFromDraft(draft: FilterDraft): FilterSpec {
  return {
    ...emptyFilterSpec(),
    prefix: blankToNull(draft.prefix),
    glob: blankToNull(draft.glob),
    min_size: joinBytes(draft.minSize, draft.minUnit),
    max_size: joinBytes(draft.maxSize, draft.maxUnit),
    modified_after: dateToTimestamp(draft.after),
    modified_before: dateToTimestamp(draft.before),
    storage_classes: draft.classes,
  };
}
