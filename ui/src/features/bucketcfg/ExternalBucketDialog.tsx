import { Text, TextInput } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";

/** The bucket names the backend accepts for an existing bucket (the legacy rules of S3). */
function nameProblem(name: string): string | null {
  if (name.length === 0) {
    return "Enter the name of the bucket.";
  }
  if (name.length > 255) {
    return "A bucket name has at most 255 characters.";
  }
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(name)) {
    return "Use letters, digits, dots, hyphens and underscores, starting with a letter or digit.";
  }
  return null;
}

/**
 * Adds a bucket that the account can use but not list (`ListBuckets` is denied, or the bucket belongs
 * to another account) to its bucket list, marked as external.
 */
export function ExternalBucketDialog({ accountId, onClose }: { accountId: string; onClose: () => void }) {
  const model = useModel();
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const problem = touched || name.length > 0 ? nameProblem(name) : null;

  const submit = async () => {
    setTouched(true);
    if (nameProblem(name) !== null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await model.api.add_external_bucket({ account_id: accountId, name });
      model.status.info(`External bucket ${name} added.`);
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
      title="Add external bucket"
      okLabel="Add bucket"
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
    >
      <Text size="sm" c="dimmed">
        Use this for a bucket that you can open but that is missing from the list: your account is not allowed to
        list buckets, or the bucket belongs to another account. It is added to the list of this account and marked
        External.
      </Text>
      <TextInput
        label="Bucket name"
        value={name}
        onChange={(event) => setName(event.currentTarget.value.trim())}
        error={problem}
        required
        spellCheck={false}
        autoComplete="off"
        data-autofocus
      />
    </OkCancelDialog>
  );
}
