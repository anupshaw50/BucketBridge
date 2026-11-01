import { Alert, Button, Group, PasswordInput, SegmentedControl, Select, Stack, Switch, Table, Text, TextInput } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import { useState } from "react";
import type { EncryptionChange, Sse } from "../../api/types";
import { useModel } from "../../app/context";
import { formatDateTime } from "../../lib/format";
import {
  CANNED_ACLS,
  generateCustomerKey,
  isArchiveClass,
  isCannedAcl,
  isStorageClass,
  STORAGE_CLASSES,
  validateCustomerKey,
} from "../../lib/objectMeta";
import { sseKeyArg, sseKeyProblem, type TabContext, useApply } from "./apply";
import { ApplyBar, CurrentKeyField } from "./parts";

/** Storage class of one object or of the selection. */
export function StorageClassTab({ ctx }: { ctx: TabContext }) {
  const model = useModel();
  const applier = useApply(ctx);
  const details = ctx.details;
  const current = details?.storage_class ?? null;
  const [value, setValue] = useState<string | null>(current);
  const restored = details?.restore !== null && details?.restore !== undefined && !details.restore.in_progress;
  const archived = details !== null && isArchiveClass(current) && !restored;
  const chosen = isStorageClass(value) ? value : null;
  const apply = () => {
    if (chosen === null) {
      return;
    }
    void applier.run(() =>
      model.api.set_object_storage_class({
        account_id: ctx.accountId,
        bucket: ctx.bucket,
        keys: ctx.keys,
        storage_class: chosen,
        sse_c_key: sseKeyArg(ctx),
      }),
    );
  };
  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        S3 changes the class by copying the object onto itself. Headers, user metadata, tags, encryption and the ACL are
        kept; the object gets a new modification time, and on a versioned bucket a new version.
      </Text>
      {archived ? (
        <Alert color="yellow" icon={<IconAlertCircle size={16} />} role="alert" py="xs">
          This object is archived ({current}) and has not been restored. Restore it before changing its storage class.
        </Alert>
      ) : null}
      {details !== null && details.restore !== null ? (
        <Text size="sm">
          {details.restore.in_progress
            ? "A restore is in progress."
            : `A restored copy is available${details.restore.expires ? ` until ${formatDateTime(details.restore.expires)}` : ""}.`}
        </Text>
      ) : null}
      <Select
        label="Storage class"
        placeholder={ctx.batch ? "Choose a class" : undefined}
        data={STORAGE_CLASSES.map((item) => ({ value: item.value, label: `${item.value}: ${item.label}` }))}
        value={value}
        onChange={setValue}
        allowDeselect={false}
        comboboxProps={{ withinPortal: false }}
      />
      <CurrentKeyField ctx={ctx} />
      <ApplyBar
        applier={applier}
        batch={ctx.batch}
        disabled={chosen === null || (!ctx.batch && chosen === current) || sseKeyProblem(ctx) !== null}
        onApply={apply}
      />
    </Stack>
  );
}

function describeSse(sse: Sse): string {
  switch (sse.type) {
    case "none":
      return "Not reported (the bucket's default applies).";
    case "s3":
      return "SSE-S3: keys managed by S3.";
    case "kms":
      return `SSE-KMS${sse.key_id ? `, key ${sse.key_id}` : ", AWS managed key"}${sse.bucket_key ? ", with an S3 Bucket Key" : ""}.`;
    case "c":
      return `SSE-C: encrypted with a customer-provided key (MD5 ${sse.key_md5}).`;
  }
}

type EncryptionKind = "s3" | "kms" | "c";

function isKind(value: string): value is EncryptionKind {
  return value === "s3" || value === "kms" || value === "c";
}

/** Server-side encryption: SSE-S3, SSE-KMS and SSE-C. */
export function EncryptionTab({ ctx }: { ctx: TabContext }) {
  const model = useModel();
  const applier = useApply(ctx);
  const current = ctx.details?.meta.sse ?? null;
  const [kind, setKind] = useState<EncryptionKind>("s3");
  const [keyId, setKeyId] = useState("");
  const [bucketKey, setBucketKey] = useState(false);
  const [newKey, setNewKey] = useState("");
  const keyIdProblem = kind === "kms" && /\s/.test(keyId.trim()) ? "A KMS key has no spaces." : null;
  const newKeyProblem = kind === "c" ? validateCustomerKey(newKey) : null;
  let change: EncryptionChange;
  if (kind === "s3") {
    change = { type: "s3" };
  } else if (kind === "kms") {
    change = { type: "kms", key_id: keyId.trim().length > 0 ? keyId.trim() : null, bucket_key: bucketKey };
  } else {
    change = { type: "c", key: newKey.trim() };
  }
  const apply = () =>
    void applier.run(
      () =>
        model.api.set_object_encryption({
          account_id: ctx.accountId,
          bucket: ctx.bucket,
          keys: ctx.keys,
          encryption: change,
          source_sse_c_key: sseKeyArg(ctx),
        }),
      // From now on the objects have the new customer key, or none.
      change.type === "c" ? change.key : "",
    );
  const generate = () => setNewKey(generateCustomerKey());
  const copyKey = async () => {
    try {
      await model.platform.copyText(newKey, { sensitive: true });
      model.status.info("The new key was copied. Keep it safe: it is the only way to read the objects again.");
    } catch (error) {
      model.status.error(error, { notify: "Could not copy to the clipboard" });
    }
  };
  return (
    <Stack gap="sm">
      {current !== null ? (
        <Text size="sm">
          <strong>Current encryption:</strong> {describeSse(current)}
        </Text>
      ) : null}
      <Text size="sm" c="dimmed">
        S3 re-encrypts by copying the object onto itself; headers, user metadata, tags, the storage class and the ACL are
        kept. Encryption cannot be removed, only changed.
      </Text>
      <SegmentedControl
        aria-label="Encryption type"
        value={kind}
        onChange={(next) => {
          if (isKind(next)) {
            setKind(next);
          }
        }}
        data={[
          { value: "s3", label: "SSE-S3" },
          { value: "kms", label: "SSE-KMS" },
          { value: "c", label: "SSE-C" },
        ]}
      />
      {kind === "kms" ? (
        <>
          <TextInput
            label="KMS key"
            description="Key ID, key ARN, alias name or alias ARN. Leave empty for the AWS managed key (aws/s3)."
            value={keyId}
            onChange={(event) => setKeyId(event.currentTarget.value)}
            error={keyIdProblem}
            autoComplete="off"
            spellCheck={false}
          />
          <Switch label="Use an S3 Bucket Key (fewer requests to KMS)" checked={bucketKey} onChange={(event) => setBucketKey(event.currentTarget.checked)} />
        </>
      ) : null}
      {kind === "c" ? (
        <Stack gap={6}>
          <PasswordInput
            label="New customer key"
            description="32 bytes: 64 hexadecimal digits, or 44 base64 characters. It is sent with the request and never stored."
            value={newKey}
            onChange={(event) => setNewKey(event.currentTarget.value)}
            error={newKey.length > 0 ? newKeyProblem : null}
            autoComplete="off"
          />
          <Group gap="xs">
            <Button size="xs" variant="default" onClick={generate}>
              Generate key
            </Button>
            <Button size="xs" variant="default" disabled={newKeyProblem !== null} onClick={() => void copyKey()}>
              Copy key
            </Button>
          </Group>
          <Text size="xs" c="dimmed">
            Keep the key safe. Without it the object cannot be read, copied or changed, and BucketBridge cannot recover it.
          </Text>
        </Stack>
      ) : null}
      <CurrentKeyField ctx={ctx} />
      <ApplyBar
        applier={applier}
        batch={ctx.batch}
        disabled={keyIdProblem !== null || newKeyProblem !== null || sseKeyProblem(ctx) !== null}
        onApply={apply}
      />
    </Stack>
  );
}

/** Canned ACLs and the grants an object has. */
export function PermissionsTab({ ctx }: { ctx: TabContext }) {
  const model = useModel();
  const applier = useApply(ctx);
  const acl = ctx.details?.acl ?? null;
  const aclError = ctx.details?.acl_error ?? null;
  const [value, setValue] = useState<string | null>(acl?.canned ?? null);
  const chosen = isCannedAcl(value) ? value : null;
  const apply = () => {
    if (chosen === null) {
      return;
    }
    void applier.run(() =>
      model.api.set_object_acl({ account_id: ctx.accountId, bucket: ctx.bucket, keys: ctx.keys, acl: chosen }),
    );
  };
  return (
    <Stack gap="sm">
      {aclError !== null ? (
        <Alert color="yellow" icon={<IconAlertCircle size={16} />} title="The ACL could not be read" role="alert">
          {aclError.message}
        </Alert>
      ) : null}
      {acl !== null ? (
        <Stack gap={4}>
          <Text size="sm">
            <strong>Current ACL:</strong> {acl.canned ?? "custom grants"}
          </Text>
          <Table withTableBorder verticalSpacing={2} fz="xs" aria-label="Grants">
            <Table.Thead>
              <Table.Tr>
                <Table.Th>Grantee</Table.Th>
                <Table.Th>Permission</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {acl.grants.map((grant) => (
                <Table.Tr key={`${grant.grantee}/${grant.permission}`}>
                  <Table.Td style={{ overflowWrap: "anywhere" }}>
                    {grant.grantee_type === "group" ? grant.grantee.split("/").pop() : grant.grantee}
                  </Table.Td>
                  <Table.Td>{grant.permission}</Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Stack>
      ) : null}
      <Text size="sm" c="dimmed">
        Buckets with ACLs disabled (Object Ownership: BucketOwnerEnforced) reject changes here; use the bucket policy
        there. Public ACLs can also be blocked by the bucket's Block Public Access settings.
      </Text>
      <Select
        label="Canned ACL"
        placeholder={ctx.batch || acl?.canned === null ? "Choose an ACL" : undefined}
        data={CANNED_ACLS.map((item) => ({ value: item.value, label: item.label }))}
        value={value}
        onChange={setValue}
        allowDeselect={false}
        comboboxProps={{ withinPortal: false }}
      />
      <ApplyBar
        applier={applier}
        batch={ctx.batch}
        disabled={chosen === null || (!ctx.batch && chosen === acl?.canned)}
        onApply={apply}
      />
    </Stack>
  );
}
