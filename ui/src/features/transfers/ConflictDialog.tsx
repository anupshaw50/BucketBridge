import { Button, Checkbox, Group, Stack, Text } from "@mantine/core";
import { useState } from "react";
import type { ConflictItem, OverwriteChoice } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { formatBytes, formatCount } from "../../lib/format";
import { KIND_LABELS } from "../../lib/transfers";

function describeItem(item: ConflictItem): string {
  const size = item.size === null ? tr("unknown size") : formatBytes(item.size);
  const time = item.modified === null ? tr("unknown time") : new Date(item.modified).toLocaleString();
  return tr("{size}, modified {time}", { size, time });
}

/**
 * Asks what to do about an existing destination for the first task that
 * waits (overwrite policy "ask"). The answer can cover the rest of the batch.
 * Mounted once, next to the other dialogs.
 */
export function ConflictHost() {
  const model = useModel();
  const pending = useSlice(model.conflicts.store, (state) => state.pending);
  const [applyToAll, setApplyToAll] = useState(false);
  const [busy, setBusy] = useState(false);
  const first = pending[0];
  if (first === undefined) {
    return null;
  }
  const { conflict } = first;
  const batch = first.group_id === null ? 0 : pending.filter((item) => item.group_id === first.group_id).length - 1;
  const answer = async (choice: OverwriteChoice) => {
    setBusy(true);
    try {
      await model.api.answer_conflict({
        task_id: first.task_id,
        choice,
        apply_to_batch: applyToAll && first.group_id !== null,
      });
    } catch (error) {
      model.status.error(error, { notify: tr("The answer could not be sent") });
      model.conflicts.remove(first.task_id);
    } finally {
      setBusy(false);
    }
  };
  const cancelTask = async () => {
    setBusy(true);
    try {
      await model.api.cancel_task({ id: first.task_id });
    } catch (error) {
      model.status.error(error, { notify: tr("The transfer could not be cancelled") });
    } finally {
      model.conflicts.remove(first.task_id);
      setBusy(false);
    }
  };
  return (
    <AppModal opened onClose={() => undefined} withCloseButton={false} closeOnEscape={false} closeOnClickOutside={false} title={tr("The destination already exists")} size="lg">
      <Stack gap="sm">
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {tr("{kind} of", { kind: KIND_LABELS[conflict.kind] })}
          <b>{conflict.source}</b> {tr("would replace")} <b>{conflict.destination}</b>.
        </Text>
        <Text size="sm" c="dimmed">
          {tr("Source: {info}", { info: describeItem(conflict.source_info) })}
        </Text>
        <Text size="sm" c="dimmed">
          {tr("Existing: {info}", { info: describeItem(conflict.destination_info) })}
        </Text>
        {pending.length > 1 ? (
          <Text size="xs" c="dimmed">
            {pending.length - 1 === 1
              ? tr("1 more transfer waits for an answer.")
              : tr("{count} more transfers wait for an answer.", { count: pending.length - 1 })}
          </Text>
        ) : null}
        {first.group_id === null ? null : (
          <Checkbox
            label={tr("Apply to all remaining in this batch")}
            description={
              batch > 0
                ? tr("{count} other transfers of this batch are waiting now; the rest follow the same answer.", {
                    count: formatCount(batch),
                  })
                : undefined
            }
            checked={applyToAll}
            onChange={(event) => setApplyToAll(event.currentTarget.checked)}
          />
        )}
        <Group justify="space-between" mt="xs">
          <Button variant="subtle" color="red" disabled={busy} onClick={() => void cancelTask()}>
            {tr("Cancel this transfer")}
          </Button>
          <Group gap="xs">
            <Button variant="default" disabled={busy} onClick={() => void answer("skip")}>
              {tr("Skip")}
            </Button>
            <Button variant="default" disabled={busy} onClick={() => void answer("rename")}>
              {tr("Rename")}
            </Button>
            <Button variant="default" disabled={busy} onClick={() => void answer("overwrite_if_newer")}>
              {tr("Overwrite if newer")}
            </Button>
            <Button disabled={busy} onClick={() => void answer("overwrite")} data-autofocus>
              {tr("Overwrite")}
            </Button>
          </Group>
        </Group>
      </Stack>
    </AppModal>
  );
}
