import {
  Alert,
  Badge,
  Button,
  Group,
  Progress,
  Radio,
  SegmentedControl,
  Select,
  Stack,
  Switch,
  Text,
  TextInput,
} from "@mantine/core";
import { IconCheck, IconFolderOpen, IconInfoCircle } from "@tabler/icons-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useMemo, useRef, useState } from "react";
import type { SyncDirection, SyncRow } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { Wizard } from "../../components/Wizard";
import { formatBytes, formatCount, formatDateTime, percent, plural } from "../../lib/format";
import { s3Uri } from "../../lib/s3keys";
import {
  ACTION_LABELS,
  DIRECTION_LABELS,
  isFailedRow,
  pendingRows,
  SIDE_LABELS,
  syncCounts,
  type SyncRowFilter,
  visibleRowIndexes,
} from "../../lib/sync";
import { S3FolderPicker } from "./S3FolderPicker";

const SYNC_ROW_HEIGHT = 26;

const DIRECTION_HELP: Record<SyncDirection, string> = {
  upload: "Copy new and changed local files to S3.",
  download: "Copy new and changed S3 objects to the local folder.",
  bidirectional: "Copy in both directions; when a file changed on both sides, the newer one wins.",
};

const ACTION_COLORS: Record<SyncRow["action"], string> = {
  copy: "blue",
  delete: "orange",
  equal: "gray",
  failed: "red",
};

function rowStatus(row: SyncRow): string {
  if (row.error !== null) {
    return row.error;
  }
  if (row.action === "equal") {
    return "In sync";
  }
  return row.done ? "Done" : "Pending";
}

function ResultsTable({ rows, filter }: { rows: SyncRow[]; filter: SyncRowFilter }) {
  const indexes = useMemo(() => visibleRowIndexes(rows, filter), [rows, filter]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: indexes.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => SYNC_ROW_HEIGHT,
    overscan: 10,
  });
  return (
    <div className="bb-sync-table" role="table" aria-label="Sync results" aria-rowcount={indexes.length + 1}>
      <div role="row" className="bb-sync-row bb-sync-head">
        <span role="columnheader">Path</span>
        <span role="columnheader">Action</span>
        <span role="columnheader">Target</span>
        <span role="columnheader">Local size</span>
        <span role="columnheader">Local modified</span>
        <span role="columnheader">S3 size</span>
        <span role="columnheader">S3 modified</span>
        <span role="columnheader">Status</span>
      </div>
      <div ref={scrollRef} className="bb-sync-scroll">
        {indexes.length === 0 ? (
          <Text size="sm" c="dimmed" p="sm">
            {filter === "failed" ? "No failed rows." : "No differences found: both sides are in sync."}
          </Text>
        ) : (
          <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((item) => {
              const row = rows[indexes[item.index] ?? -1];
              if (!row) {
                return null;
              }
              return (
                <div
                  key={row.rel_path}
                  role="row"
                  className="bb-sync-row"
                  data-failed={isFailedRow(row) || undefined}
                  style={{ transform: `translateY(${item.start}px)`, height: SYNC_ROW_HEIGHT }}
                >
                  <span role="cell" className="bb-ellipsis" title={row.rel_path}>
                    {row.rel_path}
                  </span>
                  <span role="cell">
                    <Badge size="xs" variant="light" color={ACTION_COLORS[row.action]}>
                      {ACTION_LABELS[row.action]}
                    </Badge>
                  </span>
                  <span role="cell">{row.target === null ? "" : SIDE_LABELS[row.target]}</span>
                  <span role="cell">{row.local_meta ? formatBytes(row.local_meta.size) : ""}</span>
                  <span role="cell">{formatDateTime(row.local_meta?.modified ?? null)}</span>
                  <span role="cell">{row.s3_meta ? formatBytes(row.s3_meta.size) : ""}</span>
                  <span role="cell">{formatDateTime(row.s3_meta?.last_modified ?? null)}</span>
                  <span role="cell" className="bb-ellipsis" title={rowStatus(row)} data-error={row.error !== null || undefined}>
                    {row.done && row.action !== "equal" ? <IconCheck size={12} aria-hidden /> : null} {rowStatus(row)}
                  </span>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function Results() {
  const model = useModel();
  const plan = useSlice(model.sync.store, (state) => state.plan);
  const activeAccountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const planAccount = useSlice(model.session.store, (state) =>
    state.accounts.find((account) => account.id === plan?.account_id),
  );
  const progress = useSlice(model.sync.store, (state) => state.progress);
  const running = useSlice(model.sync.store, (state) => state.running);
  const cancelling = useSlice(model.sync.store, (state) => state.cancelling);
  const error = useSlice(model.sync.store, (state) => state.error);
  const [filter, setFilter] = useState<SyncRowFilter>("all");
  if (plan === null) {
    return null;
  }
  const counts = syncCounts(plan.rows);
  const failedNow = plan.rows.filter((row) => isFailedRow(row)).length;
  // Copy and delete rows that are not done: what Execute or Resume would run.
  const pending = pendingRows(plan.rows);
  const started = progress !== null;
  const otherAccount = plan.account_id !== activeAccountId;
  return (
    <Stack gap="sm">
      <Text size="sm" fw={500}>
        {planAccount ? `Account: ${planAccount.label} (${planAccount.region})` : "Account: no longer exists"}
      </Text>
      <Text size="sm" style={{ overflowWrap: "anywhere" }}>
        <strong>{DIRECTION_LABELS[plan.direction]}</strong> between {plan.local_root} and {plan.s3_prefix}
        {plan.delete ? ", deleting extra files on the target" : ""}.
      </Text>
      <Group gap="xs" aria-label="Rows per action">
        {(["copy", "delete", "equal", "failed"] as const).map((action) => (
          <Badge key={action} variant="light" color={ACTION_COLORS[action]}>
            {ACTION_LABELS[action]}: {formatCount(counts[action])}
          </Badge>
        ))}
      </Group>
      <Group justify="space-between">
        <SegmentedControl<SyncRowFilter>
          size="xs"
          value={filter}
          onChange={setFilter}
          aria-label="Rows"
          data={[
            { value: "all", label: `All rows (${formatCount(plan.rows.length)})` },
            { value: "failed", label: `Failed only (${formatCount(failedNow)})` },
          ]}
        />
        <Group gap="xs">
          {running ? (
            <Button size="xs" color="red" variant="light" loading={cancelling} onClick={() => void model.sync.cancel()}>
              Cancel
            </Button>
          ) : (
            <Button size="xs" disabled={pending === 0 || otherAccount} onClick={() => void model.sync.execute()}>
              {started ? "Resume" : "Execute"}
            </Button>
          )}
        </Group>
      </Group>
      {progress !== null ? (
        <Stack gap={2}>
          <Progress
            value={percent(progress.done, progress.total) ?? 100}
            color={progress.failed > 0 ? "orange" : "teal"}
            animated={running}
            aria-label="Sync progress"
          />
          <Text size="xs" c="dimmed" aria-live="polite">
            {progress.finished
              ? `Finished: ${formatCount(progress.done)} of ${formatCount(progress.total)} applied, ${formatCount(
                  progress.failed,
                )} failed.${pending > 0 ? ` Resume retries ${plural(pending, "copy or delete row")} not done yet.` : ""}`
              : `Applying: ${formatCount(progress.done)} of ${formatCount(progress.total)} done, ${formatCount(
                  progress.failed,
                )} failed.`}
          </Text>
        </Stack>
      ) : pending === 0 && counts.failed > 0 ? (
        <Alert color="yellow" icon={<IconInfoCircle size={16} />} py="xs">
          Nothing to execute. Rows marked failed cannot be synchronised; their status says why. Fix the cause and analyse
          again.
        </Alert>
      ) : pending === 0 ? (
        <Alert color="teal" icon={<IconCheck size={16} />} py="xs">
          Nothing to do: the folders are in sync.
        </Alert>
      ) : null}
      {error !== null ? (
        <Alert color="red" py="xs" role="alert">
          {error}
        </Alert>
      ) : null}
      <ResultsTable rows={plan.rows} filter={filter} />
    </Stack>
  );
}

/** Folder synchronisation wizard: direction, folders, analysis and execution. */
export function SyncDialog({ onClose, at }: { onClose: () => void; at?: { bucket: string; prefix: string } }) {
  const model = useModel();
  const accountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const buckets = useSlice(model.browser.store, (state) => state.bucketList.buckets);
  const openBucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const openPrefix = useSlice(model.browser.store, (state) => state.listing.prefix);
  const storedPlan = useSlice(model.sync.store, (state) => state.plan);
  const analyzing = useSlice(model.sync.store, (state) => state.analyzing);
  const running = useSlice(model.sync.store, (state) => state.running);
  const syncError = useSlice(model.sync.store, (state) => state.error);
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  // Only a plan of the active account is reopened; another account's plan never offers Execute here.
  const plan = storedPlan !== null && storedPlan.account_id === accountId ? storedPlan : null;
  const otherRunning = storedPlan !== null && plan === null && running;
  const otherLabel = accounts.find((account) => account.id === storedPlan?.account_id)?.label ?? "another account";
  const [step, setStep] = useState(plan === null ? 0 : 2);
  const [direction, setDirection] = useState<SyncDirection>(plan?.direction ?? "upload");
  const [deleteExtra, setDeleteExtra] = useState(plan?.delete ?? false);
  const [localRoot, setLocalRoot] = useState(plan?.local_root ?? "");
  const [bucket, setBucket] = useState<string | null>(at?.bucket ?? openBucket);
  const [prefix, setPrefix] = useState(at !== undefined ? at.prefix : openBucket !== null ? openPrefix : "");
  const [touched, setTouched] = useState(false);

  const browseLocal = async () => {
    try {
      const folder = await model.platform.pickFolder("Choose the local folder");
      if (folder !== null) {
        setLocalRoot(folder);
      }
    } catch (error) {
      model.status.error(error, { notify: "The folder dialog could not be opened" });
    }
  };
  const analyse = async () => {
    setTouched(true);
    if (accountId === null || bucket === null || localRoot.length === 0) {
      return;
    }
    const ok = await model.sync.analyze({
      accountId,
      localRoot,
      s3Prefix: s3Uri(bucket, prefix),
      direction,
      deleteExtra: direction === "bidirectional" ? false : deleteExtra,
    });
    if (ok) {
      setStep(2);
    }
  };
  const next = () => {
    if (step === 0) {
      setStep(1);
      return true;
    }
    void analyse();
    return false;
  };
  const deleteHelp =
    direction === "upload"
      ? "Delete S3 objects that do not exist in the local folder."
      : direction === "download"
        ? "Delete local files that do not exist in the S3 folder."
        : "Not available in both directions: a file found on one side only is copied to the other side.";

  return (
    <AppModal opened onClose={onClose} title="Synchronise folders" size="xl" closeOnClickOutside={false}>
      {accountId === null ? (
        <Text size="sm">Add or choose an account first.</Text>
      ) : (
        <Wizard
          active={step}
          onNext={next}
          onBack={() => setStep(Math.max(0, step - 1))}
          onFinish={onClose}
          onCancel={onClose}
          nextLabel={step === 1 ? "Analyse" : "Next"}
          finishLabel="Close"
          showCancel={step < 2}
          backDisabled={running}
          busy={analyzing}
          error={step === 1 ? syncError : null}
          steps={[
            {
              label: "Direction",
              content: (
                <Stack gap="sm">
                  {otherRunning ? (
                    <Alert color="yellow" icon={<IconInfoCircle size={16} />} py="xs">
                      A synchronisation for {otherLabel} is still running. Starting a new one here stops showing its
                      progress; switch back to {otherLabel} to follow or cancel it.
                    </Alert>
                  ) : null}
                  <Radio.Group
                    label="What should be synchronised?"
                    value={direction}
                    onChange={setDirection}
                  >
                    <Stack gap="xs" mt="xs">
                      {(["upload", "download", "bidirectional"] as const).map((value) => (
                        <Radio key={value} value={value} label={DIRECTION_LABELS[value]} description={DIRECTION_HELP[value]} />
                      ))}
                    </Stack>
                  </Radio.Group>
                  <Switch
                    label="Delete extra files on the target"
                    description={deleteHelp}
                    checked={direction !== "bidirectional" && deleteExtra}
                    disabled={direction === "bidirectional"}
                    onChange={(event) => setDeleteExtra(event.currentTarget.checked)}
                  />
                </Stack>
              ),
            },
            {
              label: "Folders",
              content: (
                <Stack gap="sm">
                  <Group align="flex-end" gap="xs" wrap="nowrap">
                    <TextInput
                      label="Local folder"
                      value={localRoot}
                      readOnly
                      placeholder="Choose a folder"
                      style={{ flex: 1 }}
                      error={touched && localRoot.length === 0 ? "Choose the local folder." : null}
                    />
                    <Button variant="default" leftSection={<IconFolderOpen size={14} />} onClick={() => void browseLocal()}>
                      Browse...
                    </Button>
                  </Group>
                  <Select
                    label="Bucket"
                    placeholder="Choose a bucket"
                    data={buckets.map((item) => item.name)}
                    value={bucket}
                    searchable
                    nothingFoundMessage="No bucket with that name"
                    onChange={(value) => {
                      setBucket(value);
                      setPrefix("");
                    }}
                    error={touched && bucket === null ? "Choose a bucket." : null}
                    comboboxProps={{ withinPortal: false }}
                  />
                  {bucket !== null ? (
                    <S3FolderPicker accountId={accountId} bucket={bucket} prefix={prefix} onChange={setPrefix} />
                  ) : null}
                  <Group gap={6}>
                    <IconInfoCircle size={14} aria-hidden />
                    <Text size="xs" c="dimmed">
                      Analysing only compares the two sides; nothing is changed until you execute the plan.
                    </Text>
                  </Group>
                </Stack>
              ),
            },
            { label: "Results", content: <Results /> },
          ]}
        />
      )}
    </AppModal>
  );
}
