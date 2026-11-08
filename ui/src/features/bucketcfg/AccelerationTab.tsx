import { Button, Group, Radio, Stack, Text } from "@mantine/core";
import { useState } from "react";
import type { AccelerationStatus } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { ActionError, SectionGate, type TabProps } from "./common";
import { useAction } from "./useAction";

const STATUS_TEXT: Record<AccelerationStatus, string> = {
  unset: "Transfer acceleration has never been configured on this bucket.",
  enabled: "Transfer acceleration is enabled.",
  suspended: "Transfer acceleration is suspended.",
};

type Choice = "enabled" | "suspended";

function isChoice(value: string): value is Choice {
  return value === "enabled" || value === "suspended";
}

function AccelerationEditor({
  accountId,
  bucket,
  status,
  reload,
}: {
  accountId: string;
  bucket: string;
  status: AccelerationStatus;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const current: Choice | null = status === "unset" ? null : status;
  const [choice, setChoice] = useState<Choice | null>(current);
  const action = useAction();
  // S3 only offers acceleration for DNS-compatible names without dots.
  const dotted = bucket.includes(".");

  const save = () =>
    action.run(async () => {
      if (choice === null) {
        return;
      }
      await model.api.put_bucket_acceleration({ account_id: accountId, bucket, status: choice });
      model.status.info(tr("Transfer acceleration of {bucket} is now {choice}.", { bucket, choice: tr(choice) }));
      await reload();
    });

  return (
    <Stack gap="sm">
      <Text size="sm">{tr(STATUS_TEXT[status])}</Text>
      <Text size="sm" c="dimmed">
        {tr("Acceleration routes transfers over the AWS edge network, which helps over long distances. It is billed separately.")}
      </Text>
      <Radio.Group
        label={tr("Transfer acceleration")}
        value={choice ?? ""}
        onChange={(value) => {
          if (isChoice(value)) {
            setChoice(value);
          }
        }}
      >
        <Stack gap={6} mt={6}>
          <Radio
            value="enabled"
            label={tr("Enabled")}
            disabled={dotted}
            description={dotted ? tr("Not available: the name of this bucket contains dots.") : undefined}
          />
          <Radio value="suspended" label={tr("Suspended")} />
        </Stack>
      </Radio.Group>
      <ActionError message={action.error} />
      <Group gap="xs">
        <Button loading={action.busy} disabled={choice === null || choice === current} onClick={() => void save()}>
          {tr("Save")}
        </Button>
      </Group>
    </Stack>
  );
}

export function AccelerationTab({ accountId, bucket, section, reload }: TabProps<AccelerationStatus>) {
  return (
    <SectionGate section={section} reload={reload}>
      {(status) => <AccelerationEditor accountId={accountId} bucket={bucket} status={status} reload={reload} />}
    </SectionGate>
  );
}
