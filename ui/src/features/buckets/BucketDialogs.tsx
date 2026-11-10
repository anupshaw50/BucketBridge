import { Alert, Button, Select, Stack, Switch, Text, TextInput } from "@mantine/core";
import { IconChevronDown, IconChevronRight } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { CreateBucketOptions, ObjectOwnership, PublicAccessBlock } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { RegionInput } from "../../components/RegionInput";
import { tr } from "../../i18n";
import { partitionOf } from "../../lib/regions";
import { useRegions } from "../../lib/useRegions";
import { validateBucketName, validateRegion } from "../../lib/validation";
import { FIELDS as PUBLIC_ACCESS_FIELDS } from "../bucketcfg/publicAccessFields";

const OWNERSHIP_DEFAULT = "default";

const OWNERSHIP_CHOICES: { value: ObjectOwnership | typeof OWNERSHIP_DEFAULT; label: string }[] = [
  { value: OWNERSHIP_DEFAULT, label: tr("Server default") },
  { value: "BucketOwnerEnforced", label: tr("Bucket owner enforced (ACLs disabled, recommended)") },
  { value: "BucketOwnerPreferred", label: tr("Bucket owner preferred (ACLs enabled)") },
  { value: "ObjectWriter", label: tr("Object writer (ACLs enabled)") },
];

const ALL_BLOCKED: PublicAccessBlock = {
  block_public_acls: true,
  ignore_public_acls: true,
  block_public_policy: true,
  restrict_public_buckets: true,
};

function isOwnershipChoice(value: string | null): value is ObjectOwnership | typeof OWNERSHIP_DEFAULT {
  return OWNERSHIP_CHOICES.some((choice) => choice.value === value);
}

/**
 * Creates a bucket; the name follows the S3 naming rules, the region is optional. The advanced options
 * (Object Lock, Object Ownership, public access block with all four switches on) are sent only when
 * that section is opened, so a plain bucket is created exactly as before.
 */
export function CreateBucketDialog({ accountId, onClose }: { accountId: string; onClose: () => void }) {
  const model = useModel();
  const account = useSlice(model.session.store, (state) => state.accounts.find((item) => item.id === accountId));
  const [name, setName] = useState("");
  const [region, setRegion] = useState("");
  const regions = useRegions();
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [advanced, setAdvanced] = useState(false);
  const [objectLock, setObjectLock] = useState(false);
  const [ownership, setOwnership] = useState<ObjectOwnership | typeof OWNERSHIP_DEFAULT>(OWNERSHIP_DEFAULT);
  const [block, setBlock] = useState<PublicAccessBlock>(ALL_BLOCKED);
  const nameError = name.length > 0 || touched ? validateBucketName(name) : null;
  const regionError = region.trim().length > 0 ? validateRegion(region.trim()) : null;
  const submit = async () => {
    setTouched(true);
    if (validateBucketName(name) !== null || regionError !== null) {
      return;
    }
    if (advanced && objectLock) {
      const confirmed = await model.dialogs.confirm({
        title: tr("Turn on Object Lock"),
        message: tr("Object Lock cannot be turned off again once {name} is created, and S3 turns versioning on for it. Objects can then be protected from deletion.", { name }),
        confirmLabel: tr("Create bucket"),
        danger: true,
      });
      if (!confirmed) {
        return;
      }
    }
    const options: CreateBucketOptions | null = advanced
      ? { object_lock: objectLock, ownership: ownership === OWNERSHIP_DEFAULT ? null : ownership, public_access_block: block }
      : null;
    setBusy(true);
    setError(null);
    try {
      const result = await model.api.create_bucket({
        account_id: accountId,
        name,
        region: region.trim().length > 0 ? region.trim() : null,
        options,
      });
      model.status.info(tr("Bucket {name} created.", { name }));
      for (const warning of result.warnings) {
        model.status.info(warning, { notify: tr("Bucket {name} was created, with a warning", { name }) });
      }
      onClose();
      await model.browser.loadBuckets(accountId);
      if (model.session.store.getState().activeAccountId === accountId) {
        await model.browser.openLocation(name, "");
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };
  return (
    <OkCancelDialog
      opened
      title={tr("New bucket")}
      okLabel={tr("Create bucket")}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
    >
      <TextInput
        label={tr("Bucket name")}
        description={tr("3 to 63 characters: lowercase letters, digits, dots and hyphens.")}
        value={name}
        onChange={(event) => setName(event.currentTarget.value.trim())}
        error={nameError}
        required
        spellCheck={false}
        autoComplete="off"
        data-autofocus
      />
      <RegionInput
        label={tr("Region (optional)")}
        description={tr("Leave empty to use the account region{details}.", {
          details: account ? ` (${account.region})` : "",
        })}
        placeholder={account?.region}
        value={region}
        onChange={setRegion}
        error={regionError}
        partition={account?.endpoint === null ? partitionOf(regions, account.region) : null}
      />
      <Button
        variant="subtle"
        size="compact-sm"
        style={{ alignSelf: "flex-start" }}
        leftSection={advanced ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
        aria-expanded={advanced}
        onClick={() => setAdvanced((open) => !open)}
      >
        {tr("Advanced options")}
      </Button>
      {advanced ? (
        <Stack gap="sm">
          <Switch
            label={tr("Enable Object Lock")}
            description={tr("Lets you protect objects from being deleted or overwritten. It cannot be turned off later, and S3 turns versioning on with it.")}
            checked={objectLock}
            onChange={(event) => setObjectLock(event.currentTarget.checked)}
          />
          <Select
            label={tr("Object ownership")}
            description={tr("Who owns uploaded objects. With ACLs disabled the bucket owner owns everything and access is controlled by policies.")}
            data={OWNERSHIP_CHOICES.map((choice) => ({ value: choice.value, label: choice.label }))}
            value={ownership}
            onChange={(value) => {
              if (isOwnershipChoice(value)) {
                setOwnership(value);
              }
            }}
            allowDeselect={false}
            comboboxProps={{ withinPortal: false }}
          />
          <Stack gap={6}>
            <Text size="sm" fw={500}>
              {tr("Block public access")}
            </Text>
            {PUBLIC_ACCESS_FIELDS.map(({ key, label, description }) => (
              <Switch
                key={key}
                label={tr(label)}
                description={tr(description)}
                checked={block[key]}
                onChange={(event) => {
                  const checked = event.currentTarget.checked;
                  setBlock((current) => ({ ...current, [key]: checked }));
                }}
              />
            ))}
            {PUBLIC_ACCESS_FIELDS.some(({ key }) => !block[key]) ? (
              <Alert color="yellow" role="note" py="xs">
                <Text size="sm">{tr("With a switch off, this bucket can become public through ACLs or a bucket policy.")}</Text>
              </Alert>
            ) : null}
            <Text size="xs" c="dimmed">
              {tr("A server that does not support the public access block leaves it out and tells you.")}
            </Text>
          </Stack>
        </Stack>
      ) : null}
    </OkCancelDialog>
  );
}

/** Deletes a bucket after the user types its name; a non-empty bucket reports the conflict. */
export function DeleteBucketDialog({
  accountId,
  bucket,
  onClose,
}: {
  accountId: string;
  bucket: string;
  onClose: () => void;
}) {
  const model = useModel();
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await model.api.delete_bucket({ account_id: accountId, name: bucket });
      model.status.info(tr("Bucket {name} deleted.", { name: bucket }));
      onClose();
      if (model.browser.state.listing.bucket === bucket) {
        await model.browser.switchAccount(accountId);
      } else {
        await model.browser.loadBuckets(accountId);
      }
    } catch (raw) {
      const failure = toBbError(raw);
      setError(
        failure.code === "conflict"
          ? `${failure.message} ${tr("Delete all objects in the bucket first, then try again.")}`
          : failure.message,
      );
    } finally {
      setBusy(false);
    }
  };
  return (
    <OkCancelDialog
      opened
      title={tr("Delete bucket")}
      okLabel={tr("Delete bucket")}
      danger
      okDisabled={typed !== bucket}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
    >
      <Text size="sm">
        {tr("The bucket")} <strong style={{ overflowWrap: "anywhere" }}>{bucket}</strong>{" "}
        {tr("is deleted permanently. Only empty buckets can be deleted.")}
      </Text>
      <TextInput
        label={
          <>
            {tr("Type")} <strong>{bucket}</strong> {tr("to confirm")}
          </>
        }
        value={typed}
        onChange={(event) => setTyped(event.currentTarget.value)}
        autoComplete="off"
        spellCheck={false}
        data-autofocus
      />
    </OkCancelDialog>
  );
}
