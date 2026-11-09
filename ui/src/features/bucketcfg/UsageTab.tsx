import { Alert, Button, Checkbox, Group, Loader, Stack, Table, Text } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import { useEffect, useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type { BucketUsage, BucketVersioning, Section } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { formatBytes, formatCount } from "../../lib/format";

type RunState =
  | { status: "idle" }
  | { status: "running"; usage: BucketUsage | null; phase: "objects" | "multipart" }
  | { status: "done"; usage: BucketUsage }
  | { status: "cancelled" }
  | { status: "failed"; message: string };

let jobCounter = 0;

function newJobId(): string {
  jobCounter += 1;
  return `usage-${Date.now().toString(36)}-${jobCounter}`;
}

function exactBytes(bytes: number): string {
  return tr("{size} ({bytes} bytes)", { size: formatBytes(bytes), bytes: formatCount(bytes) });
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <Table.Tr>
      <Table.Th scope="row" fw={500}>
        {label}
      </Table.Th>
      <Table.Td>{value}</Table.Td>
    </Table.Tr>
  );
}

function Totals({ usage }: { usage: BucketUsage }) {
  return (
    <Table withRowBorders aria-label={tr("Bucket usage")}>
      <Table.Tbody>
        <Row label={tr("Objects")} value={formatCount(usage.objects)} />
        <Row label={tr("Size of objects")} value={exactBytes(usage.bytes)} />
        {usage.noncurrent !== null ? (
          <>
            <Row label={tr("Non-current versions")} value={formatCount(usage.noncurrent.versions)} />
            <Row label={tr("Size of non-current versions")} value={exactBytes(usage.noncurrent.bytes)} />
            <Row label={tr("Delete markers")} value={formatCount(usage.noncurrent.delete_markers)} />
          </>
        ) : null}
        {usage.multipart !== null ? (
          <>
            <Row label={tr("Unfinished multipart uploads")} value={formatCount(usage.multipart.uploads)} />
            <Row label={tr("Size of unfinished uploads")} value={exactBytes(usage.multipart.bytes)} />
          </>
        ) : null}
      </Table.Tbody>
    </Table>
  );
}

/**
 * The size of the bucket, computed by listing it: current objects, and, when asked for, older versions and
 * unfinished multipart uploads. A run shows its totals as they grow and can be cancelled.
 */
export function UsageTab({
  accountId,
  bucket,
  versioning,
}: {
  accountId: string;
  bucket: string;
  versioning: Section<BucketVersioning>;
}) {
  const model = useModel();
  const versioned = versioning.type === "loaded" && versioning.value.status !== "unversioned";
  const [withVersions, setWithVersions] = useState(versioned);
  const [withUploads, setWithUploads] = useState(true);
  const [run, setRun] = useState<RunState>({ status: "idle" });
  const job = useRef<string | null>(null);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      if (job.current !== null) {
        void model.api.cancel_bucket_usage({ job_id: job.current }).catch(() => undefined);
      }
    };
  }, [model]);

  useEffect(
    () =>
      model.api.listen("usage://progress", (event) => {
        if (event.job_id === job.current && mounted.current) {
          setRun({ status: "running", usage: event.usage, phase: event.phase });
        }
      }),
    [model],
  );

  const start = async () => {
    const id = newJobId();
    job.current = id;
    setRun({ status: "running", usage: null, phase: "objects" });
    try {
      const usage = await model.api.compute_bucket_usage({
        account_id: accountId,
        bucket,
        prefix: "",
        versions: withVersions,
        multipart: withUploads,
        job_id: id,
      });
      if (mounted.current && job.current === id) {
        setRun({ status: "done", usage });
      }
    } catch (raw) {
      const error = toBbError(raw);
      if (mounted.current && job.current === id) {
        setRun(error.code === "cancelled" ? { status: "cancelled" } : { status: "failed", message: error.message });
      }
    } finally {
      if (job.current === id) {
        job.current = null;
      }
    }
  };

  const cancel = () => {
    if (job.current !== null) {
      void model.api.cancel_bucket_usage({ job_id: job.current }).catch(() => undefined);
    }
  };

  const running = run.status === "running";
  return (
    <Stack gap="sm">
      <Text size="sm">
        {tr("Adds up the size of the bucket by listing it. A bucket with many objects takes a while; you can cancel.")}
      </Text>
      <Checkbox
        label={tr("Include all versions and delete markers")}
        description={
          versioned ? undefined : tr("Versioning has never been enabled on this bucket, so there is nothing more to find.")
        }
        checked={withVersions}
        disabled={running}
        onChange={(event) => setWithVersions(event.currentTarget.checked)}
      />
      <Checkbox
        label={tr("Include unfinished multipart uploads")}
        checked={withUploads}
        disabled={running}
        onChange={(event) => setWithUploads(event.currentTarget.checked)}
      />
      <Group gap="xs">
        <Button
          size="compact-md"
          disabled={running}
          onClick={() => {
            void start();
          }}
        >
          {run.status === "idle" ? tr("Calculate size") : tr("Calculate again")}
        </Button>
        {running ? (
          <>
            <Button size="compact-md" variant="default" onClick={cancel}>
              {tr("Cancel")}
            </Button>
            <Loader size="xs" aria-label={tr("Calculating")} />
            <Text size="sm" c="dimmed" role="status">
              {run.phase === "multipart"
                ? tr("Sizing unfinished multipart uploads...")
                : tr("Counting objects: {count} so far", { count: formatCount(run.usage?.objects ?? 0) })}
            </Text>
          </>
        ) : null}
      </Group>
      {run.status === "failed" ? (
        <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" title={tr("The size could not be calculated")}>
          <Text size="sm" style={{ overflowWrap: "anywhere" }}>
            {run.message}
          </Text>
        </Alert>
      ) : null}
      {run.status === "cancelled" ? (
        <Text size="sm" role="status">
          {tr("Cancelled. The totals are incomplete.")}
        </Text>
      ) : null}
      {run.status === "done" ? <Totals usage={run.usage} /> : null}
      {run.status === "running" && run.usage !== null ? <Totals usage={run.usage} /> : null}
    </Stack>
  );
}
