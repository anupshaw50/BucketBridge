import { Text, TextInput } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel, useSlice } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";
import { validateBucketName, validateRegion } from "../../lib/validation";

/** Creates a bucket; the name follows the S3 naming rules, the region is optional. */
export function CreateBucketDialog({ accountId, onClose }: { accountId: string; onClose: () => void }) {
  const model = useModel();
  const account = useSlice(model.session.store, (state) => state.accounts.find((item) => item.id === accountId));
  const [name, setName] = useState("");
  const [region, setRegion] = useState("");
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const nameError = name.length > 0 || touched ? validateBucketName(name) : null;
  const regionError = region.trim().length > 0 ? validateRegion(region.trim()) : null;
  const submit = async () => {
    setTouched(true);
    if (validateBucketName(name) !== null || regionError !== null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await model.api.create_bucket({
        account_id: accountId,
        name,
        region: region.trim().length > 0 ? region.trim() : null,
      });
      model.status.info(tr("Bucket {name} created.", { name }));
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
      <TextInput
        label={tr("Region (optional)")}
        description={tr("Leave empty to use the account region{details}.", {
          details: account ? ` (${account.region})` : "",
        })}
        placeholder={account?.region}
        value={region}
        onChange={(event) => setRegion(event.currentTarget.value)}
        error={regionError}
        spellCheck={false}
      />
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
