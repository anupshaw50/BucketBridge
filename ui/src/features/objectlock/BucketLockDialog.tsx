import { Alert, Button, NumberInput, SegmentedControl, Stack, Switch, Text } from "@mantine/core";
import { IconAlertTriangle } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { BucketObjectLock, LockMode, VersioningStatus } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";

/** Whether Object Lock is on for a bucket, and its default retention. */
export function BucketLockDialog({ accountId, bucket, onClose }: { accountId: string; bucket: string; onClose: () => void }) {
  const model = useModel();
  const [current, setCurrent] = useState<BucketObjectLock | null>(null);
  // `null` while unknown: a server that cannot tell is left to decide when the lock is turned on.
  const [versioning, setVersioning] = useState<VersioningStatus | null>(null);
  const [versioningBusy, setVersioningBusy] = useState(false);
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
    model.api
      .get_bucket_versioning({ account_id: accountId, bucket })
      .then((value) => live && setVersioning(value.status))
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [model, accountId, bucket]);

  const count = typeof amount === "number" ? amount : Number.NaN;
  const valid = !useDefault || (Number.isInteger(count) && count >= 1);
  // Object Lock can only be turned on for a bucket whose versioning is enabled.
  const needsVersioning = current !== null && !current.enabled && versioning !== null && versioning !== "enabled";

  const enableVersioning = async () => {
    setVersioningBusy(true);
    setError(null);
    try {
      await model.api.put_bucket_versioning({ account_id: accountId, bucket, status: "enabled" });
      model.status.info(tr("Versioning of {bucket} is now {choice}.", { bucket, choice: tr("enabled") }));
      setVersioning("enabled");
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setVersioningBusy(false);
    }
  };

  const submit = async () => {
    if (!valid) {
      return;
    }
    if (current?.enabled !== true) {
      const confirmed = await model.dialogs.confirm({
        title: tr("Turn on Object Lock"),
        message: tr(
          "Object Lock cannot be turned off again once it is on for {bucket}, by you or by anyone else. With Compliance retention, protected object versions cannot be deleted until their retention ends. Versioning must be enabled.",
          { bucket },
        ),
        confirmLabel: tr("Turn on"),
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
      model.status.info(tr("Object Lock settings of {bucket} saved.", { bucket }));
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
      title={tr("Object lock: {bucket}", { bucket })}
      okLabel={current?.enabled === true ? tr("Save") : tr("Turn on Object Lock")}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      okDisabled={current === null || !valid || needsVersioning}
      error={error}
    >
      <Stack gap="sm">
        <Text size="sm" data-testid="lock-state">
          {current === null
            ? tr("Reading...")
            : current.enabled
              ? tr("Object Lock is on for this bucket.")
              : tr("Object Lock is off for this bucket.")}
        </Text>
        {current !== null && !current.enabled ? (
          needsVersioning ? (
            <Alert color="yellow" icon={<IconAlertTriangle size={16} />} role="alert" title={tr("Versioning is needed first")}>
              <Text size="sm">
                {versioning === "suspended"
                  ? tr("Object Lock needs versioning, and versioning is suspended on this bucket.")
                  : tr("Object Lock needs versioning, and versioning has never been enabled on this bucket.")}
              </Text>
              <Button mt="xs" size="compact-sm" variant="default" loading={versioningBusy} onClick={() => void enableVersioning()}>
                {tr("Enable versioning")}
              </Button>
            </Alert>
          ) : (
            <Alert color="red" icon={<IconAlertTriangle size={16} />} role="note" title={tr("This cannot be undone")}>
              <Text size="sm">{tr("Once Object Lock is on, it stays on for this bucket. You will be asked to type the bucket name to confirm.")}</Text>
            </Alert>
          )
        ) : null}
        <Switch
          label={tr("Default retention for new objects")}
          checked={useDefault}
          onChange={(event) => setUseDefault(event.currentTarget.checked)}
        />
        {useDefault && (
          <>
            <SegmentedControl
              aria-label={tr("Retention mode")}
              value={mode}
              onChange={(value) => setMode(value)}
              data={[
                { value: "governance", label: tr("Governance") },
                { value: "compliance", label: tr("Compliance") },
              ]}
            />
            <NumberInput label={tr("Retain for")} value={amount} onChange={setAmount} min={1} allowDecimal={false} />
            <SegmentedControl
              aria-label={tr("Unit")}
              value={unit}
              onChange={(value) => setUnit(value)}
              data={[
                { value: "days", label: tr("Days") },
                { value: "years", label: tr("Years") },
              ]}
            />
          </>
        )}
      </Stack>
    </OkCancelDialog>
  );
}
