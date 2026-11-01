import { Button, Group, Stack, Switch, Text, TextInput } from "@mantine/core";
import { useState } from "react";
import type { LoggingTarget } from "../../api/types";
import { useModel } from "../../app/context";
import { ActionError, SectionGate, type TabProps } from "./common";
import { useAction } from "./useAction";

function LoggingEditor({
  accountId,
  bucket,
  logging,
  reload,
}: {
  accountId: string;
  bucket: string;
  logging: LoggingTarget | null;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const [enabled, setEnabled] = useState(logging !== null);
  const [targetBucket, setTargetBucket] = useState(logging?.target_bucket ?? "");
  const [prefix, setPrefix] = useState(logging?.target_prefix ?? "");
  const [touched, setTouched] = useState(false);
  const action = useAction();

  const targetProblem = enabled && touched && targetBucket.trim().length === 0 ? "Enter the bucket that receives the logs." : null;
  const sameTarget =
    logging !== null && logging.target_bucket === targetBucket.trim() && logging.target_prefix === prefix;
  const changed = enabled ? !sameTarget : logging !== null;

  const save = () =>
    action.run(async () => {
      setTouched(true);
      if (!enabled) {
        await model.api.disable_bucket_logging({ account_id: accountId, bucket });
        model.status.info(`Access logging of ${bucket} was turned off.`);
      } else {
        if (targetBucket.trim().length === 0) {
          return;
        }
        await model.api.put_bucket_logging({
          account_id: accountId,
          bucket,
          target: { target_bucket: targetBucket.trim(), target_prefix: prefix },
        });
        model.status.info(`Access logging of ${bucket} was saved.`);
      }
      await reload();
    });

  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        Server access logging delivers a log of the requests made to this bucket as objects in another bucket.
      </Text>
      <Switch
        label="Log requests made to this bucket"
        checked={enabled}
        onChange={(event) => setEnabled(event.currentTarget.checked)}
      />
      {enabled ? (
        <>
          <TextInput
            label="Target bucket"
            description="It must be in the same region and allow S3 to deliver logs."
            value={targetBucket}
            onChange={(event) => setTargetBucket(event.currentTarget.value)}
            onBlur={() => setTouched(true)}
            error={targetProblem}
            spellCheck={false}
            autoComplete="off"
          />
          <TextInput
            label="Log prefix"
            description="Added in front of the name of every log object, for example logs/."
            value={prefix}
            onChange={(event) => setPrefix(event.currentTarget.value)}
            spellCheck={false}
            autoComplete="off"
          />
        </>
      ) : null}
      <ActionError message={action.error} />
      <Group gap="xs">
        <Button loading={action.busy} disabled={!changed} onClick={() => void save()}>
          Save
        </Button>
      </Group>
    </Stack>
  );
}

export function LoggingTab({ accountId, bucket, section, reload }: TabProps<LoggingTarget | null>) {
  return (
    <SectionGate section={section} reload={reload}>
      {(logging) => <LoggingEditor accountId={accountId} bucket={bucket} logging={logging} reload={reload} />}
    </SectionGate>
  );
}
