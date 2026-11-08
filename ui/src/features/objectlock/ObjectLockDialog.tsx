import { Checkbox, SegmentedControl, Stack, Switch, Text, TextInput } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { LockMode, ObjectChange, ObjectLockState } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";
import { formatCount } from "../../lib/format";

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
  const removeQuestion =
    keys.length === 1
      ? tr("Remove the retention of 1 object?")
      : tr("Remove the retention of {n} objects?", { n: formatCount(keys.length) });

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
        title: tr("Compliance retention"),
        message: tr("Nobody, not even the root account, can shorten or remove a compliance retention before it ends. {n} will be locked.", {
          n: keys.length === 1 ? tr("1 object") : tr("{n} objects", { n: formatCount(keys.length) }),
        }),
        confirmLabel: tr("Lock"),
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
      tr("Retention saved."),
    );
  };

  const clearRetention = async () => {
    const confirmed = await model.dialogs.confirm({
      title: tr("Remove retention"),
      message: bypass
        ? tr("{question} The governance bypass is used.", { question: removeQuestion })
        : removeQuestion,
      confirmLabel: tr("Remove"),
      danger: true,
      typedConfirmation: "REMOVE",
    });
    if (confirmed) {
      await run(() => model.api.set_object_retention({ ...base, change: { action: "clear" }, bypass_governance: bypass }), tr("Retention removed."));
    }
  };

  const saveHold = () => run(() => model.api.set_object_legal_hold({ ...base, on: hold }), tr("Legal hold saved."));

  return (
    <OkCancelDialog
      opened
      title={single === undefined ? (keys.length === 1 ? tr("Lock 1 object") : tr("Lock {n} objects", { n: formatCount(keys.length) })) : tr("Lock: {key}", { key: single })}
      okLabel={tr("Set retention")}
      onOk={() => void setRetention()}
      onCancel={onClose}
      busy={busy}
      okDisabled={until === "" || !dateOk}
      error={error}
      extraActions={
        <>
          <button type="button" onClick={() => void clearRetention()} disabled={busy}>
            {tr("Remove retention")}
          </button>
          <button type="button" onClick={() => void saveHold()} disabled={busy}>
            {tr("Save legal hold")}
          </button>
        </>
      }
    >
      <Stack gap="sm">
        {state !== null && (
          <Text size="sm" data-testid="object-lock-state">
            {state.mode === null
              ? tr("No retention")
              : tr("{mode} until {until}", {
                  mode: state.mode === "governance" ? tr("Governance") : tr("Compliance"),
                  until: state.retain_until ?? "?",
                })}
            {state.legal_hold ? tr(", legal hold on") : ""}
          </Text>
        )}
        <SegmentedControl
          aria-label={tr("Retention mode")}
          value={mode}
          onChange={(value) => setMode(value)}
          data={[
            { value: "governance", label: tr("Governance") },
            { value: "compliance", label: tr("Compliance") },
          ]}
        />
        <TextInput
          label={tr("Retain until")}
          description={tr("Date and time, for example 2030-01-31T12:00")}
          value={until}
          onChange={(event) => setUntil(event.currentTarget.value)}
          error={dateOk ? null : tr("Enter a valid date and time.")}
        />
        <Checkbox
          label={tr("Bypass governance retention")}
          checked={bypass}
          onChange={(event) => setBypass(event.currentTarget.checked)}
        />
        <Switch label={tr("Legal hold")} checked={hold} onChange={(event) => setHold(event.currentTarget.checked)} />
      </Stack>
    </OkCancelDialog>
  );
}
