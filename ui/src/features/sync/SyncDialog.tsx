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
import { type ReactNode, useMemo, useRef, useState } from "react";
import type { SyncDirection, SyncJob, SyncRow } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { Wizard } from "../../components/Wizard";
import { formatBytes, formatCount, formatDateTime, percent, plural } from "../../lib/format";
import { s3Uri } from "../../lib/s3keys";
import { tr } from "../../i18n";
import {
  ACTION_LABELS,
  DIRECTION_LABELS,
  isFailedRow,
  parseS3Prefix,
  pendingRows,
  SIDE_LABELS,
  syncCounts,
  type SyncRowFilter,
  visibleRowIndexes,
} from "../../lib/sync";
import { S3FolderPicker } from "./S3FolderPicker";
import { SaveJobSection } from "./SaveJobSection";
import { SyncOptionsForm } from "./SyncOptionsForm";
import { defaultOptionsState, optionsToState, type SyncOptionsState, stateToOptions } from "./syncOptionsState";
import { SyncReportView } from "./SyncReportView";

const SYNC_ROW_HEIGHT = 26;

const DIRECTION_HELP: Record<SyncDirection, string> = {
  upload: tr("Copy new and changed local files to S3."),
  download: tr("Copy new and changed S3 objects to the local folder."),
  bidirectional: tr("Copy in both directions; when a file changed on both sides, the newer one wins."),
  s3_to_s3: tr("Copy new and changed objects from one S3 prefix to another one (same or another bucket), on the server."),
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
    return tr("In sync");
  }
  return row.done ? tr("Done") : tr("Pending");
}

function ResultsTable({ rows, filter, sourceSide }: { rows: SyncRow[]; filter: SyncRowFilter; sourceSide: boolean }) {
  const indexes = useMemo(() => visibleRowIndexes(rows, filter), [rows, filter]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: indexes.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => SYNC_ROW_HEIGHT,
    overscan: 10,
  });
  return (
    <div className="bb-sync-table" role="table" aria-label={tr("Sync results")} aria-rowcount={indexes.length + 1}>
      <div role="row" className="bb-sync-row bb-sync-head">
        <span role="columnheader">{tr("Path")}</span>
        <span role="columnheader">{tr("Action")}</span>
        <span role="columnheader">{tr("Target")}</span>
        <span role="columnheader">{sourceSide ? tr("Source size") : tr("Local size")}</span>
        <span role="columnheader">{sourceSide ? tr("Source modified") : tr("Local modified")}</span>
        <span role="columnheader">{sourceSide ? tr("Destination size") : tr("S3 size")}</span>
        <span role="columnheader">{sourceSide ? tr("Destination modified") : tr("S3 modified")}</span>
        <span role="columnheader">{tr("Status")}</span>
      </div>
      <div ref={scrollRef} className="bb-sync-scroll">
        {indexes.length === 0 ? (
          <Text size="sm" c="dimmed" p="sm">
            {filter === "failed" ? tr("No failed rows.") : tr("No differences found: both sides are in sync.")}
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

function Results({ footer }: { footer?: ReactNode }) {
  const model = useModel();
  const plan = useSlice(model.sync.store, (state) => state.plan);
  const activeAccountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const planAccount = useSlice(model.session.store, (state) =>
    state.accounts.find((account) => account.id === plan?.account_id),
  );
  const progress = useSlice(model.sync.store, (state) => state.progress);
  const report = useSlice(model.sync.store, (state) => state.report);
  const analyzing = useSlice(model.sync.store, (state) => state.analyzing);
  const running = useSlice(model.sync.store, (state) => state.running);
  const cancelling = useSlice(model.sync.store, (state) => state.cancelling);
  const error = useSlice(model.sync.store, (state) => state.error);
  const [filter, setFilter] = useState<SyncRowFilter>("all");
  if (plan === null) {
    return analyzing ? (
      <Text size="sm" role="status">
        {tr("Analysing...")}
      </Text>
    ) : error !== null ? (
      <Alert color="red" py="xs" role="alert">
        {error}
      </Alert>
    ) : null;
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
        {planAccount
          ? tr("Account: {label} ({region})", { label: planAccount.label, region: planAccount.region })
          : tr("Account: no longer exists")}
      </Text>
      <Text size="sm" style={{ overflowWrap: "anywhere" }}>
        <strong>{DIRECTION_LABELS[plan.direction]}</strong>{" "}
        {plan.direction === "s3_to_s3"
          ? tr("from {from} to {to}", { from: plan.source_prefix ?? "", to: plan.s3_prefix })
          : tr("between {local} and {remote}", { local: plan.local_root, remote: plan.s3_prefix })}
        {plan.delete ? tr(", deleting extra files on the target.") : tr(".")}
      </Text>
      <Group gap="xs" aria-label={tr("Rows per action")}>
        {(["copy", "delete", "equal", "failed"] as const).map((action) => (
          <Badge key={action} variant="light" color={ACTION_COLORS[action]}>
            {tr("{action}: {n}", { action: ACTION_LABELS[action], n: formatCount(counts[action]) })}
          </Badge>
        ))}
      </Group>
      <Group justify="space-between">
        <SegmentedControl<SyncRowFilter>
          size="xs"
          value={filter}
          onChange={setFilter}
          aria-label={tr("Rows")}
          data={[
            { value: "all", label: tr("All rows ({n})", { n: formatCount(plan.rows.length) }) },
            { value: "failed", label: tr("Failed only ({n})", { n: formatCount(failedNow) }) },
          ]}
        />
        <Group gap="xs">
          {running ? (
            <Button size="xs" color="red" variant="light" loading={cancelling} onClick={() => void model.sync.cancel()}>
              {tr("Cancel")}
            </Button>
          ) : (
            <Button size="xs" disabled={pending === 0 || otherAccount} onClick={() => void model.sync.execute()}>
              {started ? tr("Resume") : tr("Execute")}
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
            aria-label={tr("Sync progress")}
          />
          <Text size="xs" c="dimmed" aria-live="polite">
            {progress.finished
              ? `${tr("Finished: {done} of {total} applied, {failed} failed.", {
                  done: formatCount(progress.done),
                  total: formatCount(progress.total),
                  failed: formatCount(progress.failed),
                })}${
                  pending > 0
                    ? tr(" Resume retries {rows} not done yet.", { rows: plural(pending, "copy or delete row") })
                    : ""
                }`
              : tr("Applying: {done} of {total} done, {failed} failed.", {
                  done: formatCount(progress.done),
                  total: formatCount(progress.total),
                  failed: formatCount(progress.failed),
                })}
          </Text>
        </Stack>
      ) : pending === 0 && counts.failed > 0 ? (
        <Alert color="yellow" icon={<IconInfoCircle size={16} />} py="xs">
          {tr("Nothing to execute. Rows marked failed cannot be synchronised; their status says why. Fix the cause and analyse again.")}
        </Alert>
      ) : pending === 0 ? (
        <Alert color="teal" icon={<IconCheck size={16} />} py="xs">
          {tr("Nothing to do: the folders are in sync.")}
        </Alert>
      ) : null}
      {error !== null ? (
        <Alert color="red" py="xs" role="alert">
          {error}
        </Alert>
      ) : null}
      {report !== null && progress?.finished === true ? <SyncReportView report={report} /> : null}
      <ResultsTable rows={plan.rows} filter={filter} sourceSide={plan.direction === "s3_to_s3"} />
      {footer}
    </Stack>
  );
}

/** Folder synchronisation wizard: direction, folders, analysis and execution; also edits a saved sync job. */
export function SyncDialog({
  onClose,
  at,
  job,
  review,
}: {
  onClose: () => void;
  at?: { bucket: string; prefix: string };
  /** A saved job that fills the form; with an id it is edited and saved back. */
  job?: SyncJob;
  /** Opens on the results of the plan that is already in the sync state. */
  review?: boolean;
}) {
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
  const otherLabel = accounts.find((account) => account.id === storedPlan?.account_id)?.label ?? tr("another account");
  const jobTarget = job === undefined ? null : parseS3Prefix(job.s3_prefix);
  const jobSource = job?.s3_source === undefined ? null : parseS3Prefix(job.s3_source);
  const [step, setStep] = useState(job !== undefined ? 0 : plan === null && review !== true ? 0 : 2);
  const [direction, setDirection] = useState<SyncDirection>(job?.direction ?? plan?.direction ?? "upload");
  const [deleteExtra, setDeleteExtra] = useState(job?.delete ?? plan?.delete ?? false);
  const [localRoot, setLocalRoot] = useState(job?.local_root ?? plan?.local_root ?? "");
  const [bucket, setBucket] = useState<string | null>(jobTarget?.bucket ?? at?.bucket ?? openBucket);
  const [prefix, setPrefix] = useState(
    jobTarget !== null ? jobTarget.prefix : at !== undefined ? at.prefix : openBucket !== null ? openPrefix : "",
  );
  const [sourceBucket, setSourceBucket] = useState<string | null>(jobSource?.bucket ?? null);
  const [sourcePrefix, setSourcePrefix] = useState(jobSource?.prefix ?? "");
  const [options, setOptions] = useState<SyncOptionsState>(
    job === undefined ? defaultOptionsState() : optionsToState(job.options),
  );
  const [touched, setTouched] = useState(false);
  const s3ToS3 = direction === "s3_to_s3";
  const editingJob = job !== undefined && job.id !== "";

  const browseLocal = async () => {
    try {
      const folder = await model.platform.pickFolder(tr("Choose the local folder."));
      if (folder !== null) {
        setLocalRoot(folder);
      }
    } catch (error) {
      model.status.error(error, { notify: tr("The folder dialog could not be opened") });
    }
  };
  /** What is missing from the form, or null when it is complete. */
  const formProblem = (): string | null => {
    if (bucket === null) {
      return s3ToS3 ? tr("Choose the destination bucket.") : tr("Choose a bucket.");
    }
    if (s3ToS3 && sourceBucket === null) {
      return tr("Choose the source bucket.");
    }
    if (!s3ToS3 && localRoot.length === 0) {
      return tr("Choose the local folder.");
    }
    return null;
  };
  const analyse = async () => {
    setTouched(true);
    if (accountId === null || bucket === null || formProblem() !== null) {
      return;
    }
    const ok = await model.sync.analyze({
      accountId,
      localRoot: s3ToS3 ? "" : localRoot,
      s3Prefix: s3Uri(bucket, prefix),
      direction,
      deleteExtra: direction === "bidirectional" ? false : deleteExtra,
      options: stateToOptions(options),
      ...(s3ToS3 && sourceBucket !== null ? { sourcePrefix: s3Uri(sourceBucket, sourcePrefix) } : {}),
    });
    if (ok) {
      setStep(2);
    }
  };
  const buildJob = (name: string): SyncJob | string => {
    setTouched(true);
    const problem = formProblem();
    if (accountId === null || bucket === null || problem !== null) {
      return problem ?? tr("Choose an account first.");
    }
    return {
      id: "",
      name,
      account_id: editingJob ? job.account_id : accountId,
      direction,
      local_root: s3ToS3 ? "" : localRoot,
      s3_prefix: s3Uri(bucket, prefix),
      ...(s3ToS3 && sourceBucket !== null ? { s3_source: s3Uri(sourceBucket, sourcePrefix) } : {}),
      delete: direction === "bidirectional" ? false : deleteExtra,
      options: stateToOptions(options),
    };
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
    direction === "upload" || direction === "s3_to_s3"
      ? direction === "upload"
        ? tr("Delete S3 objects that do not exist in the local folder.")
        : tr("Delete objects in the destination that do not exist in the source.")
      : direction === "download"
        ? tr("Delete local files that do not exist in the S3 folder.")
        : tr("Not available in both directions: a file found on one side only is copied to the other side.");
  const bucketNames = buckets.map((item) => item.name);

  return (
    <AppModal
      opened
      onClose={onClose}
      title={editingJob ? tr("Edit sync job {name}", { name: job.name }) : tr("Synchronise folders")}
      size="xl"
      closeOnClickOutside={false}
    >
      {accountId === null ? (
        <Text size="sm">{tr("Add or choose an account first.")}</Text>
      ) : (
        <Wizard
          active={step}
          onNext={next}
          onBack={() => setStep(Math.max(0, step - 1))}
          onFinish={onClose}
          onCancel={onClose}
          nextLabel={step === 1 ? tr("Analyse") : tr("Next")}
          finishLabel={tr("Close")}
          showCancel={step < 2}
          backDisabled={running}
          busy={analyzing}
          error={step === 1 ? syncError : null}
          steps={[
            {
              label: tr("Direction"),
              content: (
                <Stack gap="sm">
                  {otherRunning ? (
                    <Alert color="yellow" icon={<IconInfoCircle size={16} />} py="xs">
                      {tr(
                        "A synchronisation for {label} is still running. Starting a new one here stops showing its progress; switch back to {label} to follow or cancel it.",
                        { label: otherLabel },
                      )}
                    </Alert>
                  ) : null}
                  <Radio.Group label={tr("What should be synchronised?")} value={direction} onChange={setDirection}>
                    <Stack gap="xs" mt="xs">
                      {(["upload", "download", "bidirectional", "s3_to_s3"] as const).map((value) => (
                        <Radio
                          key={value}
                          value={value}
                          label={DIRECTION_LABELS[value]}
                          description={DIRECTION_HELP[value]}
                        />
                      ))}
                    </Stack>
                  </Radio.Group>
                  <Switch
                    label={tr("Delete extra files on the target")}
                    description={deleteHelp}
                    checked={direction !== "bidirectional" && deleteExtra}
                    disabled={direction === "bidirectional"}
                    onChange={(event) => setDeleteExtra(event.currentTarget.checked)}
                  />
                </Stack>
              ),
            },
            {
              label: tr("Folders"),
              content: (
                <Stack gap="sm">
                  {s3ToS3 ? (
                    <>
                      <Select
                        label={tr("Source bucket")}
                        placeholder={tr("Choose a bucket")}
                        data={bucketNames}
                        value={sourceBucket}
                        searchable
                        nothingFoundMessage={tr("No bucket with that name")}
                        onChange={(value) => {
                          setSourceBucket(value);
                          setSourcePrefix("");
                        }}
                        error={touched && sourceBucket === null ? tr("Choose the source bucket.") : null}
                        comboboxProps={{ withinPortal: false }}
                      />
                      {sourceBucket !== null ? (
                        <S3FolderPicker accountId={accountId} bucket={sourceBucket} prefix={sourcePrefix} onChange={setSourcePrefix} />
                      ) : null}
                    </>
                  ) : (
                    <Group align="flex-end" gap="xs" wrap="nowrap">
                      <TextInput
                        label={tr("Local folder")}
                        value={localRoot}
                        readOnly
                        placeholder={tr("Choose a folder")}
                        style={{ flex: 1 }}
                        error={touched && localRoot.length === 0 ? tr("Choose the local folder.") : null}
                      />
                      <Button variant="default" leftSection={<IconFolderOpen size={14} />} onClick={() => void browseLocal()}>
                        {tr("Browse...")}
                      </Button>
                    </Group>
                  )}
                  <Select
                    label={s3ToS3 ? tr("Destination bucket") : tr("Bucket")}
                    placeholder={tr("Choose a bucket")}
                    data={bucketNames}
                    value={bucket}
                    searchable
                    nothingFoundMessage={tr("No bucket with that name")}
                    onChange={(value) => {
                      setBucket(value);
                      setPrefix("");
                    }}
                    error={touched && bucket === null ? tr("Choose a bucket.") : null}
                    comboboxProps={{ withinPortal: false }}
                  />
                  {bucket !== null ? (
                    <S3FolderPicker accountId={accountId} bucket={bucket} prefix={prefix} onChange={setPrefix} />
                  ) : null}
                  <SyncOptionsForm value={options} onChange={setOptions} />
                  <SaveJobSection existing={editingJob ? job : null} build={buildJob} />
                  <Group gap={6}>
                    <IconInfoCircle size={14} aria-hidden />
                    <Text size="xs" c="dimmed">
                      {tr("Analysing only compares the two sides; nothing is changed until you execute the plan.")}
                    </Text>
                  </Group>
                </Stack>
              ),
            },
            { label: tr("Results"), content: <Results footer={<SaveJobSection existing={null} build={buildJob} />} /> },
          ]}
        />
      )}
    </AppModal>
  );
}
