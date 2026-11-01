import { NumberInput, SegmentedControl, Stack, Switch, Text } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { BucketObjectLock, LockMode } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";

/** Whether Object Lock is on for a bucket, and its default retention. */
export function BucketLockDialog({ accountId, bucket, onClose }: { accountId: string; bucket: string; onClose: () => void }) {
  const model = useModel();
  const [current, setCurrent] = useState<BucketObjectLock | null>(null);
  const [useDefault, setUseDefault] = useState(false);
  const [mode, setMode] = useState<LockMode>("governance");
  const [unit, setUnit] = useState<"days" | "years">("days");
  const [amount, setAmount] = useState<number | string>(1);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    model.api
      .get_bucket_object_lock({ account_id: accountId, bucket })
      .then((value) => {
        if (!live) {
          return;
        }
        setCurrent(value);
        const rule = value.default_retention;
        if (rule !== null) {
          setUseDefault(true);
          setMode(rule.mode);
          setUnit(rule.days === null ? "years" : "days");
          setAmount(rule.days ?? rule.years ?? 1);
        }
      })
      .catch((raw: unknown) => live && setError(toBbError(raw).message));
    return () => {
      live = false;
    };
  }, [model, accountId, bucket]);

  const count = typeof amount === "number" ? amount : Number.NaN;
  const valid = !useDefault || (Number.isInteger(count) && count >= 1);

  const submit = async () => {
    if (!valid) {
      return;
    }
    if (current?.enabled !== true) {
      const confirmed = await model.dialogs.confirm({
        title: "Turn on Object Lock",
        message: `Object Lock cannot be turned off again once it is on for ${bucket}. Versioning must be enabled.`,
        confirmLabel: "Turn on",
        danger: true,
        typedConfirmation: bucket,
      });
      if (!confirmed) {
        return;
      }
    }
    setBusy(true);
    setError(null);
    try {
      await model.api.put_bucket_object_lock({
        account_id: accountId,
        bucket,
        default_retention: useDefault
          ? { mode, days: unit === "days" ? count : null, years: unit === "years" ? count : null }
          : null,
      });
      model.status.info(`Object Lock settings of ${bucket} saved.`);
      onClose();
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <OkCancelDialog
      opened
      title={`Object lock: ${bucket}`}
      okLabel={current?.enabled === true ? "Save" : "Turn on Object Lock"}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      okDisabled={current === null || !valid}
      error={error}
    >
      <Stack gap="sm">
        <Text size="sm" data-testid="lock-state">
          {current === null ? "Reading..." : current.enabled ? "Object Lock is on for this bucket." : "Object Lock is off for this bucket."}
        </Text>
        <Switch label="Default retention for new objects" checked={useDefault} onChange={(event) => setUseDefault(event.currentTarget.checked)} />
        {useDefault && (
          <>
            <SegmentedControl
              aria-label="Retention mode"
              value={mode}
              onChange={(value) => setMode(value)}
              data={[
                { value: "governance", label: "Governance" },
                { value: "compliance", label: "Compliance" },
              ]}
            />
            <NumberInput label="Retain for" value={amount} onChange={setAmount} min={1} allowDecimal={false} />
            <SegmentedControl
              aria-label="Unit"
              value={unit}
              onChange={(value) => setUnit(value)}
              data={[
                { value: "days", label: "Days" },
                { value: "years", label: "Years" },
              ]}
            />
          </>
        )}
      </Stack>
    </OkCancelDialog>
  );
}
