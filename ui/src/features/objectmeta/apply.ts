import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { ObjectChange, ObjectDetails } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { formatCount } from "../../lib/format";
import { validateCustomerKey } from "../../lib/objectMeta";

/** What every tab of the properties dialog gets. */
export interface TabContext {
  accountId: string;
  bucket: string;
  /** The objects the tab changes (one in single mode). */
  keys: string[];
  /** The loaded details of the object; `null` in batch mode. */
  details: ObjectDetails | null;
  /** Several objects are selected: the tab edits a patch instead of the values of one object. */
  batch: boolean;
  /** Key of objects encrypted with SSE-C, typed by the user; never stored. */
  sseKey: string;
  setSseKey: (value: string) => void;
  /**
   * Called after a change: refreshes the details and the listing. `sseKey`
   * is the key the objects have from now on (empty: none), when the change
   * moved them to or from SSE-C.
   */
  changed: (sseKey?: string) => void;
}

/** One-line summary of a batch: "2 changed, 1 unchanged, 1 failed". */
export function summarize(results: readonly ObjectChange[]): string {
  const failed = results.filter((result) => result.error !== null).length;
  const changed = results.filter((result) => result.changed).length;
  const unchanged = results.length - failed - changed;
  const base = tr("{changed} changed, {unchanged} unchanged", {
    changed: formatCount(changed),
    unchanged: formatCount(unchanged),
  });
  return failed > 0 ? tr("{summary}, {failed} failed", { summary: base, failed: formatCount(failed) }) : base;
}

export interface Applier {
  busy: boolean;
  results: ObjectChange[] | null;
  error: string | null;
  run: (call: () => Promise<ObjectChange[]>, sseKey?: string) => Promise<void>;
}

/** Runs a change command and keeps its per-object results for display. */
export function useApply(ctx: TabContext): Applier {
  const model = useModel();
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<ObjectChange[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const run = async (call: () => Promise<ObjectChange[]>, sseKey?: string) => {
    setBusy(true);
    setError(null);
    setResults(null);
    try {
      const list = await call();
      setResults(list);
      if (list.some((result) => result.changed)) {
        ctx.changed(sseKey);
      }
      model.status.info(
        list.length === 1
          ? tr("1 object: {summary}.", { summary: summarize(list) })
          : tr("{n} objects: {summary}.", { n: formatCount(list.length), summary: summarize(list) }),
      );
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };
  return { busy, results, error, run };
}

/** The SSE-C key to send: the typed one, or `null` when the field is empty. */
export function sseKeyArg(ctx: TabContext): string | null {
  const key = ctx.sseKey.trim();
  return key.length === 0 ? null : key;
}

/** Why the typed SSE-C key cannot be used; `null` when it is empty or fine. */
export function sseKeyProblem(ctx: TabContext): string | null {
  return ctx.sseKey.trim().length === 0 ? null : validateCustomerKey(ctx.sseKey);
}

/** Splits "a, b" and "a\nb" into trimmed, distinct, non-empty names. */
export function splitNames(text: string): string[] {
  return [...new Set(text.split(/[,\n]/).map((name) => name.trim()).filter((name) => name.length > 0))];
}
