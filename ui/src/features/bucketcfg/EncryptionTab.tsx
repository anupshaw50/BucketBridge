import { Button, Group, Radio, Stack, Switch, Text, TextInput } from "@mantine/core";
import { useState } from "react";
import type { BucketEncryption } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { validateKmsKeyId } from "../../lib/bucketProps";
import { ActionError, LazyTab } from "./common";
import { useAction } from "./useAction";

type Choice = "AES256" | "aws:kms";

function isChoice(value: string): value is Choice {
  return value === "AES256" || value === "aws:kms";
}

function describe(current: BucketEncryption | null): string {
  if (current === null) {
    return tr("No default encryption is configured. On AWS, S3 then encrypts new objects with SSE-S3.");
  }
  switch (current.algorithm) {
    case "AES256":
      return tr("New objects are encrypted with SSE-S3 (keys managed by S3).");
    case "aws:kms":
      return current.kms_key_id === null
        ? tr("New objects are encrypted with SSE-KMS and the AWS managed key (aws/s3).")
        : tr("New objects are encrypted with SSE-KMS and the key {key}.", { key: current.kms_key_id });
    case "aws:kms:dsse":
      return tr("New objects use dual-layer SSE-KMS. It cannot be set here; you can replace it with SSE-S3 or SSE-KMS.");
  }
}

function EncryptionEditor({
  accountId,
  bucket,
  current,
  reload,
}: {
  accountId: string;
  bucket: string;
  current: BucketEncryption | null;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const [choice, setChoice] = useState<Choice>(current?.algorithm === "aws:kms" ? "aws:kms" : "AES256");
  const [key, setKey] = useState(current?.kms_key_id ?? "");
  const [bucketKey, setBucketKey] = useState(current?.bucket_key_enabled ?? false);
  const action = useAction();
  const keyProblem = choice === "aws:kms" ? validateKmsKeyId(key) : null;

  const wanted: BucketEncryption =
    choice === "AES256"
      ? { algorithm: "AES256", kms_key_id: null, bucket_key_enabled: false }
      : { algorithm: "aws:kms", kms_key_id: key.length > 0 ? key : null, bucket_key_enabled: bucketKey };
  const changed = current === null || JSON.stringify(current) !== JSON.stringify(wanted);

  const save = () =>
    action.run(async () => {
      await model.api.put_bucket_encryption({ account_id: accountId, bucket, config: wanted });
      model.status.info(tr("The default encryption of {bucket} was saved.", { bucket }));
      await reload();
    });
  const remove = async () => {
    const confirmed = await model.dialogs.confirm({
      title: tr("Remove default encryption"),
      message: tr("The default encryption setting of {bucket} is removed. New objects get the encryption S3 applies on its own (SSE-S3 on AWS); existing objects are not changed.", { bucket }),
      confirmLabel: tr("Remove configuration"),
      danger: true,
    });
    if (!confirmed) {
      return;
    }
    await action.run(async () => {
      await model.api.delete_bucket_encryption({ account_id: accountId, bucket });
      model.status.info(tr("The default encryption of {bucket} was removed.", { bucket }));
      await reload();
    });
  };

  return (
    <Stack gap="sm">
      <Text size="sm" data-testid="encryption-state">
        {describe(current)}
      </Text>
      <Text size="xs" c="dimmed">
        {tr("The setting applies to objects uploaded from now on. Objects that are already in the bucket keep their encryption.")}
      </Text>
      <Radio.Group
        label={tr("Default encryption")}
        value={choice}
        onChange={(value) => {
          if (isChoice(value)) {
            setChoice(value);
          }
        }}
      >
        <Stack gap={6} mt={6}>
          <Radio value="AES256" label={tr("SSE-S3")} description={tr("Amazon S3 managed keys.")} />
          <Radio value="aws:kms" label={tr("SSE-KMS")} description={tr("AWS Key Management Service keys.")} />
        </Stack>
      </Radio.Group>
      {choice === "aws:kms" ? (
        <Stack gap="xs">
          <TextInput
            label={tr("KMS key")}
            description={tr("A key ID, key ARN, alias name (alias/name) or alias ARN. Leave empty for the AWS managed key (aws/s3).")}
            value={key}
            onChange={(event) => setKey(event.currentTarget.value)}
            error={keyProblem}
            autoComplete="off"
            spellCheck={false}
          />
          <Switch
            label={tr("Use an S3 Bucket Key")}
            description={tr("Lowers the cost of SSE-KMS by reducing the requests to AWS KMS.")}
            checked={bucketKey}
            onChange={(event) => setBucketKey(event.currentTarget.checked)}
          />
        </Stack>
      ) : null}
      <ActionError message={action.error} />
      <Group gap="xs">
        <Button loading={action.busy} disabled={!changed || keyProblem !== null} onClick={() => void save()}>
          {tr("Save")}
        </Button>
        {current !== null ? (
          <Button color="red" variant="light" ml="auto" disabled={action.busy} onClick={() => void remove()}>
            {tr("Remove configuration")}
          </Button>
        ) : null}
      </Group>
    </Stack>
  );
}

export function EncryptionTab({ accountId, bucket }: { accountId: string; bucket: string }) {
  const model = useModel();
  return (
    <LazyTab read={() => model.api.get_bucket_encryption({ account_id: accountId, bucket })}>
      {(current, reload) => <EncryptionEditor accountId={accountId} bucket={bucket} current={current} reload={reload} />}
    </LazyTab>
  );
}
