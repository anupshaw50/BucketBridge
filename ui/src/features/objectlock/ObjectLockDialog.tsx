import { Checkbox, SegmentedControl, Stack, Switch, Text, TextInput } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { LockMode, ObjectChange, ObjectLockState } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { plural } from "../../lib/format";

function failures(results: readonly ObjectChange[]): string | null {
  const failed = results.filter((result) => result.error !== null);
  return failed.length === 0 ? null : failed.map((result) => `${result.key}: ${result.error?.message ?? ""}`).join("\n");
}

/** Retention and legal hold quick actions for one or several objects. */
export function ObjectLockDialog({
  accountId,
  bucket,
  keys,
  onClose,
}: {
  accountId: string;
  bucket: string;
  keys: string[];
  onClose: () => void;
}) {
  const model = useModel();
  const [state, setState] = useState<ObjectLockState | null>(null);
  const [mode, setMode] = useState<LockMode>("governance");
  const [until, setUntil] = useState("");
  const [bypass, setBypass] = useState(false);
  const [hold, setHold] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const single = keys.length === 1 ? keys[0] : undefined;

  useEffect(() => {
    if (single === undefined) {
      return;
    }
    let live = true;
    model.api
      .get_object_lock({ account_id: accountId, bucket, key: single, version_id: null })
      .then((value) => {
        if (live) {
          setState(value);
          setHold(value.legal_hold);
        }
      })
      .catch((raw: unknown) => live && setError(toBbError(raw).message));
    return () => {
      live = false;
    };
  }, [model, accountId, bucket, single]);

  const base = { account_id: accountId, bucket, keys, version_id: null };
  const dateOk = until === "" || !Number.isNaN(Date.parse(until));

  const run = async (work: () => Promise<ObjectChange[]>, done: string) => {
    setBusy(true);
    setError(null);
    try {
      const results = await work();
      const problem = failures(results);
      if (problem === null) {
        model.status.info(done);
        onClose();
      } else {
        setError(problem);
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  const setRetention = async () => {
    if (until === "" || !dateOk) {
      return;
    }
    if (mode === "compliance") {
      const confirmed = await model.dialogs.confirm({
        title: "Compliance retention",
        message: `Nobody, not even the root account, can shorten or remove a compliance retention before it ends. ${plural(keys.length, "object")} will be locked.`,
        confirmLabel: "Lock",
        danger: true,
        typedConfirmation: "COMPLIANCE",
      });
      if (!confirmed) {
        return;
      }
    }
    await run(
      () =>
        model.api.set_object_retention({
          ...base,
          change: { action: "set", mode, retain_until: new Date(until).toISOString().replace(/\.\d+Z$/, "Z") },
          bypass_governance: bypass,
        }),
      "Retention saved.",
    );
  };

  const clearRetention = async () => {
    const confirmed = await model.dialogs.confirm({
      title: "Remove retention",
      message: `Remove the retention of ${plural(keys.length, "object")}?${bypass ? " The governance bypass is used." : ""}`,
      confirmLabel: "Remove",
      danger: true,
      typedConfirmation: "REMOVE",
    });
    if (confirmed) {
      await run(() => model.api.set_object_retention({ ...base, change: { action: "clear" }, bypass_governance: bypass }), "Retention removed.");
    }
  };

  const saveHold = () => run(() => model.api.set_object_legal_hold({ ...base, on: hold }), "Legal hold saved.");

  return (
    <OkCancelDialog
      opened
      title={single === undefined ? `Lock ${plural(keys.length, "object")}` : `Lock: ${single}`}
      okLabel="Set retention"
      onOk={() => void setRetention()}
      onCancel={onClose}
      busy={busy}
      okDisabled={until === "" || !dateOk}
      error={error}
      extraActions={
        <>
          <button type="button" onClick={() => void clearRetention()} disabled={busy}>
            Remove retention
          </button>
          <button type="button" onClick={() => void saveHold()} disabled={busy}>
            Save legal hold
          </button>
        </>
      }
    >
      <Stack gap="sm">
        {state !== null && (
          <Text size="sm" data-testid="object-lock-state">
            {state.mode === null ? "No retention" : `${state.mode} until ${state.retain_until ?? "?"}`}
            {state.legal_hold ? ", legal hold on" : ""}
          </Text>
        )}
        <SegmentedControl
          aria-label="Retention mode"
          value={mode}
          onChange={(value) => setMode(value)}
          data={[
            { value: "governance", label: "Governance" },
            { value: "compliance", label: "Compliance" },
          ]}
        />
        <TextInput
          label="Retain until"
          description="Date and time, for example 2030-01-31T12:00"
          value={until}
          onChange={(event) => setUntil(event.currentTarget.value)}
          error={dateOk ? null : "Enter a valid date and time."}
        />
        <Checkbox label="Bypass governance retention" checked={bypass} onChange={(event) => setBypass(event.currentTarget.checked)} />
        <Switch label="Legal hold" checked={hold} onChange={(event) => setHold(event.currentTarget.checked)} />
      </Stack>
    </OkCancelDialog>
  );
}
