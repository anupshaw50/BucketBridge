import { Button, Group, Select, Stack, Text } from "@mantine/core";
import { useState } from "react";
import type { AclGrant, BucketAcl, CannedAcl } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { AclGrantsEditor } from "../objectmeta/AclGrantsEditor";
import { ActionError, LazyTab } from "./common";
import { useAction } from "./useAction";

type BucketCanned = "private" | "public-read" | "public-read-write" | "authenticated-read";

const CANNED: readonly { value: BucketCanned; label: string }[] = [
  { value: "private", label: tr("Private") },
  { value: "public-read", label: tr("Public read (everyone can list the bucket)") },
  { value: "public-read-write", label: tr("Public read and write") },
  { value: "authenticated-read", label: tr("Authenticated read (any AWS account)") },
];

function isBucketCanned(value: string | null): value is BucketCanned {
  return CANNED.some((item) => item.value === value);
}

function AclEditor({
  accountId,
  bucket,
  info,
  reload,
}: {
  accountId: string;
  bucket: string;
  info: BucketAcl;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const current = info.acl.canned;
  const [choice, setChoice] = useState<BucketCanned | null>(isBucketCanned(current) ? current : null);
  const action = useAction();

  const apply = async () => {
    if (choice === null) {
      return;
    }
    if (choice === "public-read" || choice === "public-read-write") {
      const confirmed = await model.dialogs.confirm({
        title: tr("Make the bucket public"),
        message:
          choice === "public-read-write"
            ? tr("The ACL public-read-write lets anyone on the internet list, upload, replace and delete objects in {bucket}. The public access block may still stop it.", { bucket })
            : tr("The ACL public-read lets anyone on the internet list the objects in {bucket}. The public access block may still stop it.", { bucket }),
        confirmLabel: tr("Apply ACL"),
        danger: true,
      });
      if (!confirmed) {
        return;
      }
    }
    await action.run(async () => {
      const acl: CannedAcl = choice;
      await model.api.set_bucket_acl({ account_id: accountId, bucket, acl });
      model.status.info(tr("The ACL of {bucket} is now {acl}.", { bucket, acl }));
      await reload();
    });
  };
  const saveGrants = async (grants: AclGrant[]) => {
    await model.api.put_bucket_acl_grants({ account_id: accountId, bucket, grants });
    model.status.info(tr("The ACL grants of {bucket} were saved.", { bucket }));
    await reload();
  };

  return (
    <Stack gap="sm">
      <Text size="sm" data-testid="bucket-acl-state">
        <strong>{tr("Current ACL:")}</strong> {current ?? tr("custom grants")}
        {info.ownership !== null ? ` (${tr("Object Ownership: {ownership}", { ownership: info.ownership })})` : ""}
      </Text>
      <AclGrantsEditor
        key={JSON.stringify(info.acl.grants)}
        scope="bucket"
        grants={info.acl.grants}
        disabled={info.acls_disabled}
        onSave={saveGrants}
      />
      {info.acls_disabled ? null : (
        <Stack gap="xs">
          <Select
            label={tr("Canned ACL")}
            description={tr("Replaces every grant with a ready-made set.")}
            placeholder={current === null ? tr("Choose an ACL") : undefined}
            data={CANNED.map((item) => ({ value: item.value, label: item.label }))}
            value={choice}
            onChange={(value) => setChoice(isBucketCanned(value) ? value : null)}
            allowDeselect={false}
            comboboxProps={{ withinPortal: false }}
          />
          <ActionError message={action.error} />
          <Group gap="xs">
            <Button loading={action.busy} disabled={choice === null || choice === current} onClick={() => void apply()}>
              {tr("Apply ACL")}
            </Button>
          </Group>
        </Stack>
      )}
    </Stack>
  );
}

export function AclTab({ accountId, bucket }: { accountId: string; bucket: string }) {
  const model = useModel();
  return (
    <LazyTab read={() => model.api.get_bucket_acl({ account_id: accountId, bucket })}>
      {(info, reload) => <AclEditor accountId={accountId} bucket={bucket} info={info} reload={reload} />}
    </LazyTab>
  );
}
