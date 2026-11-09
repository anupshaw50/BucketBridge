import { Text, TextInput } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { s3Uri } from "../../lib/s3keys";

const MAX_LABEL = 100;

function labelProblem(label: string, required: boolean): string | null {
  if (required && label.trim() === "") {
    return tr("Enter a name for the bookmark.");
  }
  if ([...label.trim()].length > MAX_LABEL) {
    return tr("Use at most {n} characters.", { n: MAX_LABEL });
  }
  return null;
}

/** Saves a location as a bookmark (`bookmarkId` unset) or renames one. */
export function BookmarkDialog({
  accountId,
  bucket,
  prefix,
  bookmarkId,
  label: initial,
  onClose,
}: {
  accountId: string;
  bucket: string;
  prefix: string;
  bookmarkId?: string;
  label?: string;
  onClose: () => void;
}) {
  const model = useModel();
  const renaming = bookmarkId !== undefined;
  const [label, setLabel] = useState(initial ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const problem = labelProblem(label, renaming);

  const submit = async () => {
    if (problem !== null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (bookmarkId === undefined) {
        await model.bookmarks.add(accountId, bucket, prefix, label.trim() === "" ? null : label.trim());
        model.status.info(tr("Bookmark added."));
      } else {
        await model.bookmarks.rename(bookmarkId, label.trim());
        model.status.info(tr("Bookmark renamed."));
      }
      onClose();
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <OkCancelDialog
      opened
      title={renaming ? tr("Rename bookmark") : tr("Add bookmark")}
      okLabel={renaming ? tr("Rename") : tr("Add bookmark")}
      onOk={() => void submit()}
      onCancel={onClose}
      okDisabled={problem !== null}
      busy={busy}
      error={error}
    >
      <Text size="sm" c="dimmed" style={{ wordBreak: "break-all" }}>
        {s3Uri(bucket, prefix)}
      </Text>
      <TextInput
        label={tr("Name")}
        placeholder={renaming ? undefined : tr("Defaults to the location")}
        value={label}
        onChange={(event) => setLabel(event.currentTarget.value)}
        error={label === "" ? null : problem}
        spellCheck={false}
        autoComplete="off"
        data-autofocus
      />
    </OkCancelDialog>
  );
}
