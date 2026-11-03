import { Button, Checkbox, Group, Stack, Text } from "@mantine/core";
import { useState } from "react";
import type { ConflictItem, OverwriteChoice } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { formatBytes, plural } from "../../lib/format";
import { KIND_LABELS } from "../../lib/transfers";

function describeItem(item: ConflictItem): string {
  const size = item.size === null ? "unknown size" : formatBytes(item.size);
  const time = item.modified === null ? "unknown time" : new Date(item.modified).toLocaleString();
  return `${size}, modified ${time}`;
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
      model.status.error(error, { notify: "The answer could not be sent" });
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
      model.status.error(error, { notify: "The transfer could not be cancelled" });
    } finally {
      model.conflicts.remove(first.task_id);
      setBusy(false);
    }
  };
  return (
    <AppModal opened onClose={() => undefined} withCloseButton={false} closeOnEscape={false} closeOnClickOutside={false} title="The destination already exists" size="lg">
      <Stack gap="sm">
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {KIND_LABELS[conflict.kind]} of <b>{conflict.source}</b> would replace <b>{conflict.destination}</b>.
        </Text>
        <Text size="sm" c="dimmed">
          Source: {describeItem(conflict.source_info)}
        </Text>
        <Text size="sm" c="dimmed">
          Existing: {describeItem(conflict.destination_info)}
        </Text>
        {pending.length > 1 ? (
          <Text size="xs" c="dimmed">
            {plural(pending.length - 1, "more transfer")} wait for an answer.
          </Text>
        ) : null}
        {first.group_id === null ? null : (
          <Checkbox
            label="Apply to all remaining in this batch"
            description={batch > 0 ? `${plural(batch, "other transfer")} of this batch wait now; the rest follow the same answer.` : undefined}
            checked={applyToAll}
            onChange={(event) => setApplyToAll(event.currentTarget.checked)}
          />
        )}
        <Group justify="space-between" mt="xs">
          <Button variant="subtle" color="red" disabled={busy} onClick={() => void cancelTask()}>
            Cancel this transfer
          </Button>
          <Group gap="xs">
            <Button variant="default" disabled={busy} onClick={() => void answer("skip")}>
              Skip
            </Button>
            <Button variant="default" disabled={busy} onClick={() => void answer("rename")}>
              Rename
            </Button>
            <Button variant="default" disabled={busy} onClick={() => void answer("overwrite_if_newer")}>
              Overwrite if newer
            </Button>
            <Button disabled={busy} onClick={() => void answer("overwrite")} data-autofocus>
              Overwrite
            </Button>
          </Group>
        </Group>
      </Stack>
    </AppModal>
  );
}
