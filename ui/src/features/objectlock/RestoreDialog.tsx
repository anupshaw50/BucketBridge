import { NumberInput, SegmentedControl, Stack, Text } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { ArchiveState, RestoreTier } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";
import { formatCount } from "../../lib/format";
import { restoreText } from "./restoreText";

/** Restores archived objects: tier and days, and the current restore state of one object. */
export function RestoreDialog({
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
  const [tier, setTier] = useState<RestoreTier>("standard");
  const [days, setDays] = useState<number | string>(7);
  const [state, setState] = useState<ArchiveState | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const single = keys.length === 1 ? keys[0] : undefined;
  const count = typeof days === "number" ? days : Number.NaN;

  useEffect(() => {
    if (single === undefined) {
      return;
    }
    let live = true;
    model.api
      .get_restore_state({ account_id: accountId, bucket, key: single, version_id: null })
      .then((value) => live && setState(value))
      .catch((raw: unknown) => live && setError(toBbError(raw).message));
    return () => {
      live = false;
    };
  }, [model, accountId, bucket, single]);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const results = await model.api.restore_objects({
        account_id: accountId,
        bucket,
        keys,
        days: count,
        tier,
        version_id: null,
      });
      const failed = results.filter((result) => result.error !== null);
      if (failed.length > 0) {
        setError(failed.map((result) => `${result.key}: ${result.error?.message ?? ""}`).join("\n"));
      } else {
        model.status.info(
          keys.length === 1
            ? tr("Restore requested for 1 object.")
            : tr("Restore requested for {n} objects.", { n: formatCount(keys.length) }),
        );
        onClose();
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <OkCancelDialog
      opened
      title={tr("Restore from Glacier")}
      okLabel={tr("Request restore")}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      okDisabled={!Number.isInteger(count) || count < 1}
      error={error}
    >
      <Stack gap="sm">
        {state !== null && <Text size="sm" data-testid="restore-state">{restoreText(state)}</Text>}
        <SegmentedControl
          aria-label={tr("Tier")}
          value={tier}
          onChange={(value) => setTier(value)}
          data={[
            { value: "expedited", label: tr("Expedited") },
            { value: "standard", label: tr("Standard") },
            { value: "bulk", label: tr("Bulk") },
          ]}
        />
        <NumberInput label={tr("Keep the restored copy for (days)")} value={days} onChange={setDays} min={1} max={3650} allowDecimal={false} />
      </Stack>
    </OkCancelDialog>
  );
}
