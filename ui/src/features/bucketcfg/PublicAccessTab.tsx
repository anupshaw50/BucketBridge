import { Button, Group, Stack, Switch, Text } from "@mantine/core";
import { useState } from "react";
import type { PublicAccessBlock } from "../../api/types";
import { useModel } from "../../app/context";
import { ActionError, SectionGate, type TabProps } from "./common";
import { useAction } from "./useAction";

const FIELDS: { key: keyof PublicAccessBlock; label: string; description: string }[] = [
  {
    key: "block_public_acls",
    label: "Block new public ACLs",
    description: "Requests that would make the bucket or an object public through an ACL are rejected (BlockPublicAcls).",
  },
  {
    key: "ignore_public_acls",
    label: "Ignore public ACLs",
    description: "Public ACLs that already exist have no effect (IgnorePublicAcls).",
  },
  {
    key: "block_public_policy",
    label: "Block new public bucket policies",
    description: "A bucket policy that would make the bucket public is rejected (BlockPublicPolicy).",
  },
  {
    key: "restrict_public_buckets",
    label: "Restrict access to buckets with a public policy",
    description:
      "A public policy only gives access to the owner's account and to AWS services (RestrictPublicBuckets).",
  },
];

const ALL_OFF: PublicAccessBlock = {
  block_public_acls: false,
  ignore_public_acls: false,
  block_public_policy: false,
  restrict_public_buckets: false,
};

function PublicAccessEditor({
  accountId,
  bucket,
  block,
  reload,
}: {
  accountId: string;
  bucket: string;
  block: PublicAccessBlock | null;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const saved = block ?? ALL_OFF;
  const [draft, setDraft] = useState<PublicAccessBlock>(saved);
  const action = useAction();
  const changed = FIELDS.some(({ key }) => draft[key] !== saved[key]);

  const save = () =>
    action.run(async () => {
      await model.api.put_public_access_block({ account_id: accountId, bucket, config: draft });
      model.status.info(`The public access block of ${bucket} was saved.`);
      await reload();
    });
  const remove = async () => {
    const confirmed = await model.dialogs.confirm({
      title: "Remove public access block",
      message: `The public access block of ${bucket} is removed, so S3 stops blocking public access to it. The bucket is then only as private as its ACLs and policy make it.`,
      confirmLabel: "Remove configuration",
      danger: true,
    });
    if (!confirmed) {
      return;
    }
    await action.run(async () => {
      await model.api.delete_public_access_block({ account_id: accountId, bucket });
      model.status.info(`The public access block of ${bucket} was removed.`);
      await reload();
    });
  };

  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {block === null
          ? "No public access block is configured, so S3 blocks nothing on this bucket."
          : "These switches limit how public the bucket and its objects can become."}
      </Text>
      {FIELDS.map(({ key, label, description }) => (
        <Switch
          key={key}
          label={label}
          description={description}
          checked={draft[key]}
          onChange={(event) => {
            const checked = event.currentTarget.checked;
            setDraft((current) => ({ ...current, [key]: checked }));
          }}
        />
      ))}
      <ActionError message={action.error} />
      <Group gap="xs">
        <Button loading={action.busy} disabled={!changed} onClick={() => void save()}>
          Save
        </Button>
        {block !== null ? (
          <Button color="red" variant="light" ml="auto" disabled={action.busy} onClick={() => void remove()}>
            Remove configuration
          </Button>
        ) : null}
      </Group>
    </Stack>
  );
}

export function PublicAccessTab({ accountId, bucket, section, reload }: TabProps<PublicAccessBlock | null>) {
  return (
    <SectionGate section={section} reload={reload}>
      {(block) => <PublicAccessEditor accountId={accountId} bucket={bucket} block={block} reload={reload} />}
    </SectionGate>
  );
}
