import { useCallback, useState } from "react";
import { toBbError } from "../../api/errors";

export interface Action {
  /** A request of this tab is running. */
  busy: boolean;
  /** The message of the last failed request, shown next to the buttons. */
  error: string | null;
  /** Runs `work`; a failure becomes `error` instead of an unhandled rejection. */
  run: (work: () => Promise<void>) => Promise<void>;
  clearError: () => void;
}

/** Busy flag and error message of the requests one tab sends. */
export function useAction(): Action {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = useCallback(async (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  }, []);
  const clearError = useCallback(() => setError(null), []);
  return { busy, error, run, clearError };
}
