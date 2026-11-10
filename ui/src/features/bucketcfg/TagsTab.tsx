import { Button, Group, Stack, Text } from "@mantine/core";
import { useState } from "react";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { MAX_BUCKET_TAGS, validateBucketTagRows } from "../../lib/bucketProps";
import { type Row, recordToRows, rowsToRecord } from "../../lib/objectMeta";
import { KeyValueEditor } from "../objectmeta/parts";
import { ActionError, LazyTab } from "./common";
import { useAction } from "./useAction";

function sameTags(a: Readonly<Record<string, string>>, b: Readonly<Record<string, string>>): boolean {
  return JSON.stringify(Object.entries(a).sort()) === JSON.stringify(Object.entries(b).sort());
}

function TagsEditor({
  accountId,
  bucket,
  saved,
  reload,
}: {
  accountId: string;
  bucket: string;
  saved: Record<string, string>;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const [rows, setRows] = useState<Row[]>(() => recordToRows(saved));
  const action = useAction();
  const problem = validateBucketTagRows(rows);
  const wanted = rowsToRecord(rows);
  const changed = !sameTags(wanted, saved);

  const save = () =>
    action.run(async () => {
      await model.api.put_bucket_tags({ account_id: accountId, bucket, tags: wanted });
      model.status.info(
        rows.length === 0
          ? tr("The tags of {bucket} were removed.", { bucket })
          : tr("The tags of {bucket} were saved.", { bucket }),
      );
      await reload();
    });

  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {tr(
          "Tags label a bucket for cost allocation and organisation. A bucket can have up to {n} tags. Keys and values may contain letters, digits, spaces and + - = . _ : / @. Activate a tag as a cost allocation tag in the AWS Billing console to see it in cost reports.",
          { n: MAX_BUCKET_TAGS },
        )}
      </Text>
      <KeyValueEditor rows={rows} onChange={setRows} keyLabel={tr("Tag key")} valueLabel={tr("Tag value")} addLabel={tr("Add tag")} />
      {problem !== null ? (
        <Text size="sm" c="red" role="alert">
          {problem}
        </Text>
      ) : null}
      <ActionError message={action.error} />
      <Group gap="xs">
        <Button loading={action.busy} disabled={!changed || problem !== null} onClick={() => void save()}>
          {rows.length === 0 && Object.keys(saved).length > 0 ? tr("Remove all tags") : tr("Save")}
        </Button>
      </Group>
    </Stack>
  );
}

export function TagsTab({ accountId, bucket }: { accountId: string; bucket: string }) {
  const model = useModel();
  return (
    <LazyTab read={() => model.api.get_bucket_tags({ account_id: accountId, bucket })}>
      {(saved, reload) => <TagsEditor accountId={accountId} bucket={bucket} saved={saved} reload={reload} />}
    </LazyTab>
  );
}
