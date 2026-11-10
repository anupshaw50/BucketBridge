import type { LifecycleConfig, LifecycleRule } from "../../api/types";
import { pretty } from "./pretty";

/** The storage classes a lifecycle transition can target (`TRANSITION_CLASSES` in the core). */
export const TRANSITION_CLASSES = ["STANDARD_IA", "ONEZONE_IA", "INTELLIGENT_TIERING", "GLACIER_IR", "GLACIER", "DEEP_ARCHIVE"] as const;

/** A rule with every field at its default. */
export function emptyRule(): LifecycleRule {
  return {
    id: null,
    status: "enabled",
    filter: { prefix: null, tags: [], object_size_greater_than: null, object_size_less_than: null },
    transitions: [],
    expiration: null,
    noncurrent_version_transitions: [],
    noncurrent_version_expiration: null,
    abort_incomplete_multipart_upload_days: null,
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** `raw` over `defaults`; anything that is not an object is taken as an empty one. */
function withDefaults<T extends object>(defaults: T, raw: unknown): T {
  return { ...defaults, ...(isRecord(raw) ? raw : {}) };
}

/**
 * A rule as the form wants it: omitted fields take the defaults (the backend does the same), and what the form does
 * not know is kept as it is so the form never drops a field.
 */
export function normalizeRule(raw: unknown): LifecycleRule {
  const base = emptyRule();
  const rule = withDefaults(base, raw);
  const filter = withDefaults(base.filter, rule.filter);
  return {
    ...rule,
    filter: { ...filter, tags: Array.isArray(filter.tags) ? filter.tags : [] },
    transitions: Array.isArray(rule.transitions) ? rule.transitions.map((entry) => withDefaults({ days: null, date: null, storage_class: "" }, entry)) : [],
    expiration: isRecord(rule.expiration) ? withDefaults({ days: null, date: null, expired_object_delete_marker: false }, rule.expiration) : null,
    noncurrent_version_transitions: Array.isArray(rule.noncurrent_version_transitions)
      ? rule.noncurrent_version_transitions.map((entry) => withDefaults({ noncurrent_days: 0, newer_noncurrent_versions: null, storage_class: "" }, entry))
      : [],
    noncurrent_version_expiration: isRecord(rule.noncurrent_version_expiration)
      ? withDefaults({ noncurrent_days: null, newer_noncurrent_versions: null }, rule.noncurrent_version_expiration)
      : null,
  };
}

export type ParsedRules = { ok: true; config: LifecycleConfig } | { ok: false; message: string };

/** Reads the JSON text of the editor into rules for the form; an empty text is no rules. */
export function parseRules(text: string): ParsedRules {
  if (text.trim().length === 0) {
    return { ok: true, config: { rules: [] } };
  }
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    return { ok: false, message: error instanceof Error ? error.message : String(error) };
  }
  if (!isRecord(value) || (value.rules !== undefined && !Array.isArray(value.rules))) {
    return { ok: false, message: 'the document must be an object with a "rules" list' };
  }
  const rules = Array.isArray(value.rules) ? value.rules : [];
  return { ok: true, config: { rules: rules.map(normalizeRule) } };
}

/** The JSON text for rules. */
export function rulesText(config: LifecycleConfig): string {
  return pretty(config);
}

/**
 * Adds `pasted` rules to `existing`. A pasted rule whose id is taken gets the suffix `-copy` (then `-copy-2`, ...),
 * so a paste never overwrites a rule.
 */
export function mergeRules(existing: LifecycleRule[], pasted: LifecycleRule[]): LifecycleRule[] {
  const used = new Set(existing.flatMap((rule) => (rule.id === null ? [] : [rule.id])));
  const merged = [...existing];
  for (const rule of pasted) {
    let id = rule.id;
    if (id !== null && used.has(id)) {
      let candidate = `${id}-copy`;
      for (let n = 2; used.has(candidate); n += 1) {
        candidate = `${id}-copy-${n}`;
      }
      id = candidate;
    }
    if (id !== null) {
      used.add(id);
    }
    merged.push({ ...rule, id });
  }
  return merged;
}

/** Rules copied in this window, waiting to be pasted into a bucket (the same place for every bucket). */
let clipboard: LifecycleRule[] | null = null;

/** Keeps `rules` for a paste. */
export function copyRules(rules: LifecycleRule[]): void {
  clipboard = structuredClone(rules);
}

/** The rules that were copied, or `null`. */
export function copiedRules(): LifecycleRule[] | null {
  return clipboard === null ? null : structuredClone(clipboard);
}

/** Forgets the copied rules (tests). */
export function clearCopiedRules(): void {
  clipboard = null;
}
