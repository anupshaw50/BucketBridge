import { ActionIcon, Badge, Button, Group, Stack, Text, Tooltip } from "@mantine/core";
import { IconPencil, IconPlayerPlay, IconTrash } from "@tabler/icons-react";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { DIRECTION_LABELS } from "../../lib/sync";
import type { SyncJob } from "../../api/types";

function fromSide(job: SyncJob): string {
  return job.direction === "s3_to_s3" ? (job.s3_source ?? "") : job.local_root;
}

/** The saved sync jobs: run one (analyses it and opens the plan for review), edit it, or delete it. */
export function SyncJobsDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const jobs = useSlice(model.sync.store, (state) => state.jobs);
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const activeAccountId = useSlice(model.session.store, (state) => state.activeAccountId);

  const run = async (job: SyncJob) => {
    onClose();
    model.status.info(`Analysing the sync job ${job.name}...`);
    if (await model.sync.runJob(job.id)) {
      model.dialogs.open({ kind: "sync", review: true });
    } else {
      model.status.error(new Error(model.sync.state.error ?? "The sync job could not be analysed."), {
        notify: "The sync job could not be analysed",
      });
    }
  };
  const remove = async (job: SyncJob) => {
    const confirmed = await model.dialogs.confirm({
      title: "Delete sync job",
      message: `Delete the sync job "${job.name}"?\n\nOnly the saved setup is removed; no file or object is changed.`,
      confirmLabel: "Delete",
      danger: true,
    });
    if (!confirmed) {
      return;
    }
    try {
      await model.sync.deleteJob(job.id);
      model.status.info("Sync job deleted.");
    } catch (error) {
      model.status.error(error, { notify: "The sync job could not be deleted" });
    }
  };

  return (
    <AppModal opened onClose={onClose} title="Sync jobs" size="xl">
      <Stack gap="sm">
        {jobs.length === 0 ? (
          <Text size="sm" c="dimmed">
            No saved sync jobs. Set up a sync with Folder sync and choose Save as job.
          </Text>
        ) : (
          <ul className="bb-sync-jobs" aria-label="Saved sync jobs" style={{ listStyle: "none", margin: 0, padding: 0 }}>
            {jobs.map((job) => {
              const account = accounts.find((item) => item.id === job.account_id);
              const otherAccount = job.account_id !== activeAccountId;
              return (
                <li
                  key={job.id}
                  style={{ display: "flex", gap: 8, alignItems: "center", padding: "6px 0", borderBottom: "1px solid var(--mantine-color-default-border)" }}
                >
                  <Stack gap={2} style={{ flex: 1, minWidth: 0 }}>
                    <Group gap="xs">
                      <Text fw={600} size="sm">
                        {job.name}
                      </Text>
                      <Badge size="xs" variant="light">
                        {DIRECTION_LABELS[job.direction]}
                      </Badge>
                      {job.delete ? (
                        <Badge size="xs" variant="light" color="orange">
                          Deletes extra files
                        </Badge>
                      ) : null}
                    </Group>
                    <Text size="xs" c="dimmed" className="bb-ellipsis" title={`${fromSide(job)} to ${job.s3_prefix}`}>
                      {fromSide(job)} to {job.s3_prefix}
                      {account ? ` (${account.label})` : " (account no longer exists)"}
                    </Text>
                  </Stack>
                  <Tooltip label={otherAccount ? `Switch to ${account?.label ?? "its account"} to run this job` : "Analyse and review"}>
                    <Button
                      size="xs"
                      variant="light"
                      leftSection={<IconPlayerPlay size={14} />}
                      disabled={otherAccount}
                      aria-label={`Run ${job.name}`}
                      onClick={() => void run(job)}
                    >
                      Run
                    </Button>
                  </Tooltip>
                  <Tooltip label="Edit job">
                    <ActionIcon
                      variant="subtle"
                      color="gray"
                      aria-label={`Edit ${job.name}`}
                      onClick={() => {
                        onClose();
                        model.dialogs.open({ kind: "sync", job });
                      }}
                    >
                      <IconPencil size={16} />
                    </ActionIcon>
                  </Tooltip>
                  <Tooltip label="Delete job">
                    <ActionIcon variant="subtle" color="gray" aria-label={`Delete ${job.name}`} onClick={() => void remove(job)}>
                      <IconTrash size={16} />
                    </ActionIcon>
                  </Tooltip>
                </li>
              );
            })}
          </ul>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            Close
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
