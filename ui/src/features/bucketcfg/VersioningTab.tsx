import { Badge, Button, Group, Radio, Stack, Text } from "@mantine/core";
import { useState } from "react";
import type { BucketVersioning, VersioningStatus } from "../../api/types";
import { useModel } from "../../app/context";
import { ActionError, SectionGate, type TabProps } from "./common";
import { useAction } from "./useAction";

const STATUS_TEXT: Record<VersioningStatus, string> = {
  unversioned: "Versioning has never been enabled on this bucket.",
  enabled: "Versioning is enabled: every change to an object keeps the previous version.",
  suspended: "Versioning is suspended: no new versions are kept, existing versions stay.",
};

type Choice = "enabled" | "suspended";

function isChoice(value: string): value is Choice {
  return value === "enabled" || value === "suspended";
}

function VersioningEditor({
  accountId,
  bucket,
  versioning,
  reload,
}: {
  accountId: string;
  bucket: string;
  versioning: BucketVersioning;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const current: Choice | null = versioning.status === "unversioned" ? null : versioning.status;
  const [choice, setChoice] = useState<Choice | null>(current);
  const action = useAction();

  const save = () =>
    action.run(async () => {
      if (choice === null) {
        return;
      }
      await model.api.put_bucket_versioning({ account_id: accountId, bucket, status: choice });
      model.status.info(`Versioning of ${bucket} is now ${choice}.`);
      await reload();
    });

  return (
    <Stack gap="sm">
      <Text size="sm">{STATUS_TEXT[versioning.status]}</Text>
      <Group gap="xs">
        <Text size="sm" fw={500}>
          MFA delete
        </Text>
        <Badge variant="light" color={versioning.mfa_delete ? "teal" : "gray"}>
          {versioning.mfa_delete ? "Enabled" : "Disabled"}
        </Badge>
        <Text size="xs" c="dimmed">
          Shown for information; it cannot be changed here.
        </Text>
      </Group>
      <Radio.Group
        label="Versioning"
        value={choice ?? ""}
        onChange={(value) => {
          if (isChoice(value)) {
            setChoice(value);
          }
        }}
      >
        <Stack gap={6} mt={6}>
          <Radio value="enabled" label="Enabled" description="Keep every version of every object." />
          <Radio
            value="suspended"
            label="Suspended"
            description="Stop creating new versions. Versioning cannot be switched off completely once it was enabled."
          />
        </Stack>
      </Radio.Group>
      <ActionError message={action.error} />
      <Group gap="xs">
        <Button loading={action.busy} disabled={choice === null || choice === current} onClick={() => void save()}>
          Save
        </Button>
      </Group>
    </Stack>
  );
}

export function VersioningTab({ accountId, bucket, section, reload }: TabProps<BucketVersioning>) {
  return (
    <SectionGate section={section} reload={reload}>
      {(versioning) => (
        <VersioningEditor accountId={accountId} bucket={bucket} versioning={versioning} reload={reload} />
      )}
    </SectionGate>
  );
}
