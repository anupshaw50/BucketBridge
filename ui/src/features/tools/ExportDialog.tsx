import { Alert, Button, Checkbox, Group, Radio, Stack, Switch, Text } from "@mantine/core";
import { IconCheck, IconFolderSearch } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { ExportOutcome, ListingFormat } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { formatBytes, plural } from "../../lib/format";
import { s3Uri } from "../../lib/s3keys";
import { exportFileName, FORMAT_EXTENSIONS, FORMAT_LABELS } from "../../lib/tools";

const FORMATS: readonly { value: ListingFormat; description: string }[] = [
  { value: "csv", description: "Plain text for any tool or script. Text that starts with = + - or @ gets a leading apostrophe." },
  { value: "xlsx", description: "An Excel workbook with a header row. A listing past 1,048,576 rows continues on more sheets." },
  { value: "parquet", description: "A compact columnar file for data tools, with Snappy compression." },
];

function isFormat(value: string): value is ListingFormat {
  return FORMATS.some((item) => item.value === value);
}

export interface ExportDialogProps {
  accountId: string;
  bucket: string;
  prefix: string;
  onClose: () => void;
}

/** Writes the listing of the open folder to a CSV, Excel or Parquet file chosen in the native save dialog. */
export function ExportDialog({ accountId, bucket, prefix, onClose }: ExportDialogProps) {
  const model = useModel();
  const filter = useSlice(model.browser.store, (state) => state.filter);
  const flat = useSlice(model.browser.store, (state) => state.listing.flat);
  const [format, setFormat] = useState<ListingFormat>("csv");
  // A flat view already shows everything below the folder.
  const [recursive, setRecursive] = useState(flat);
  const [useFilter, setUseFilter] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<ExportOutcome | null>(null);

  const start = async () => {
    let path: string | null;
    try {
      path = await model.platform.pickSavePath({
        title: "Export listing",
        defaultPath: exportFileName(bucket, prefix, format),
        filters: [{ name: FORMAT_LABELS[format], extensions: [FORMAT_EXTENSIONS[format]] }],
      });
    } catch (raw) {
      model.status.error(raw, { notify: "The file dialog could not be opened" });
      return;
    }
    if (path === null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await model.api.export_listing({
        account_id: accountId,
        bucket,
        prefix,
        recursive,
        format,
        path,
        filter: filter !== null && useFilter ? filter.spec : null,
      });
      setOutcome(result);
      model.status.info(`Exported ${plural(result.rows, "row")} to ${result.path}.`);
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  const reveal = async (path: string) => {
    try {
      await model.platform.revealPath(path);
    } catch (raw) {
      model.status.error(raw, { notify: "The folder could not be shown" });
    }
  };

  return (
    <AppModal opened onClose={onClose} title="Export listing" size="md" closeOnClickOutside={!busy}>
      {outcome !== null ? (
        <Stack gap="sm">
          <Alert color="teal" icon={<IconCheck size={16} />} title="Export finished">
            <Text size="sm" style={{ overflowWrap: "anywhere" }}>
              Exported {plural(outcome.rows, "row")} ({formatBytes(outcome.bytes)}) to {outcome.path}.
            </Text>
          </Alert>
          <Group justify="flex-end" gap="xs">
            <Button variant="default" leftSection={<IconFolderSearch size={14} />} onClick={() => void reveal(outcome.path)}>
              Show in folder
            </Button>
            <Button onClick={onClose}>Close</Button>
          </Group>
        </Stack>
      ) : (
        <Stack gap="sm">
          <Text size="sm" style={{ overflowWrap: "anywhere" }}>
            Writes the listing of <strong>{s3Uri(bucket, prefix)}</strong> to a file with the columns key, size,
            last_modified, etag and storage_class.
          </Text>
          <Radio.Group
            label="File format"
            value={format}
            onChange={(value) => {
              if (isFormat(value)) {
                setFormat(value);
              }
            }}
          >
            <Stack gap="xs" mt="xs">
              {FORMATS.map((item) => (
                <Radio key={item.value} value={item.value} label={FORMAT_LABELS[item.value]} description={item.description} />
              ))}
            </Stack>
          </Radio.Group>
          <Switch
            label="Include everything below this folder"
            description="Off: only what the folder view shows, one level."
            checked={recursive}
            onChange={(event) => setRecursive(event.currentTarget.checked)}
          />
          {filter !== null ? (
            <Checkbox
              label={`Only entries matching the active filter "${filter.name}"`}
              checked={useFilter}
              onChange={(event) => setUseFilter(event.currentTarget.checked)}
            />
          ) : null}
          {error !== null ? (
            <Alert color="red" role="alert" py="xs">
              {error}
            </Alert>
          ) : null}
          <Group justify="flex-end" gap="xs">
            <Button variant="default" onClick={onClose}>
              Cancel
            </Button>
            <Button loading={busy} onClick={() => void start()}>
              Export to file...
            </Button>
          </Group>
        </Stack>
      )}
    </AppModal>
  );
}
