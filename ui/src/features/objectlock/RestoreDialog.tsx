import { NumberInput, SegmentedControl, Stack, Text } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { ArchiveState, RestoreEstimate, RestoreTier } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";
import { formatBytes, formatCount } from "../../lib/format";
import { onlyIntelligentTiering, restoreText, tierEstimateText, tierName } from "./restoreText";

/** Restores archived objects: the size and the cost per tier, tier and days, and the restore state of one object. */
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
  const [estimate, setEstimate] = useState<RestoreEstimate | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const single = keys.length === 1 ? keys[0] : undefined;
  const count = typeof days === "number" ? days : Number.NaN;
  // Intelligent-Tiering archive tiers are restored without days and without a tier.
  const tiering = estimate !== null && onlyIntelligentTiering(estimate);

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

  useEffect(() => {
    let live = true;
    model.api
      .estimate_restore({ account_id: accountId, bucket, keys })
      .then((value) => live && setEstimate(value))
      .catch((raw: unknown) => live && setError(toBbError(raw).message));
    return () => {
      live = false;
    };
  }, [model, accountId, bucket, keys]);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const results = await model.api.restore_objects({
        account_id: accountId,
        bucket,
        keys,
        days: tiering ? null : count,
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

  const unsupported = estimate?.tiers.find((entry) => entry.tier === tier)?.unsupported ?? 0;

  return (
    <OkCancelDialog
      opened
      title={tr("Restore from Glacier")}
      okLabel={tr("Request restore")}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      okDisabled={!tiering && (!Number.isInteger(count) || count < 1)}
      error={error}
    >
      <Stack gap="sm">
        {state !== null && <Text size="sm" data-testid="restore-state">{restoreText(state)}</Text>}
        {estimate !== null && (
          <Stack gap={2} data-testid="restore-estimate">
            <Text size="sm" data-testid="restore-totals">
              {tr("{n} archived objects, {size} in total.", {
                n: formatCount(estimate.objects),
                size: formatBytes(estimate.bytes),
              })}
            </Text>
            {estimate.summary.not_archived + estimate.summary.unreadable > 0 && (
              <Text size="xs" c="dimmed">
                {tr("{n} selected objects are not archived or could not be read; they are not counted.", {
                  n: formatCount(estimate.summary.not_archived + estimate.summary.unreadable),
                })}
              </Text>
            )}
            {estimate.summary.truncated && (
              <Text size="xs" c="dimmed">
                {tr("Only the first 1,000 objects are counted in the estimate.")}
              </Text>
            )}
            {!tiering &&
              estimate.tiers.map((entry) => (
                <Text size="sm" key={entry.tier} data-testid={`restore-estimate-${entry.tier}`}>
                  {tierEstimateText(entry)}
                </Text>
              ))}
            <Text size="xs" c="dimmed" data-testid="restore-estimate-note">
              {tr("Approximate, list prices as of {date}, check your AWS bill.", { date: estimate.price_date })}
            </Text>
          </Stack>
        )}
        {tiering ? (
          <Text size="sm" data-testid="restore-tiering">
            {tr("Objects in an Intelligent-Tiering archive tier are restored to the Frequent Access tier. There is no tier and no number of days to choose, and no retrieval fee.")}
          </Text>
        ) : (
          <>
            <SegmentedControl
              aria-label={tr("Tier")}
              value={tier}
              onChange={(value) => setTier(value)}
              data={(["expedited", "standard", "bulk"] as const).map((value) => ({ value, label: tierName(value) }))}
            />
            {unsupported > 0 && (
              <Text size="xs" c="red" data-testid="restore-unsupported">
                {tr("{n} of the selected objects cannot be restored with this tier.", { n: formatCount(unsupported) })}
              </Text>
            )}
            <NumberInput
              label={tr("Keep the restored copy for (days)")}
              value={days}
              onChange={setDays}
              min={1}
              max={3650}
              allowDecimal={false}
            />
            {estimate !== null && estimate.summary.groups.some((group) => group.kind.startsWith("intelligent")) && (
              <Text size="xs" c="dimmed">
                {tr("The number of days does not apply to objects in an Intelligent-Tiering archive tier.")}
              </Text>
            )}
          </>
        )}
      </Stack>
    </OkCancelDialog>
  );
}
