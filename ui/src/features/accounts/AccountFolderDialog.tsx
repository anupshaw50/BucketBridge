import { TextInput } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel, useSlice } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";
import { addFolder, folderNameTaken, renameFolder } from "../../lib/accountGroups";

const MAX_NAME = 100;

/** Asks for the name of a new account folder, or the new name of one. */
export function AccountFolderDialog({ folder, onClose }: { folder?: string; onClose: () => void }) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const folders = useSlice(model.session.store, (state) => state.folders);
  const [name, setName] = useState(folder ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trimmed = name.trim();
  const problem =
    trimmed.length > MAX_NAME
      ? tr("The folder name must be at most {n} characters.", { n: MAX_NAME })
      : folderNameTaken(folders, trimmed, folder ?? null)
        ? tr("A folder with this name already exists.")
        : null;

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      const layout =
        folder === undefined ? addFolder(accounts, folders, trimmed) : renameFolder(accounts, folders, folder, trimmed);
      await model.api.set_account_layout({ layout });
      await model.session.refreshAccounts();
      onClose();
    } catch (raw) {
      setError(toBbError(raw).message);
      setBusy(false);
    }
  };

  return (
    <OkCancelDialog
      opened
      title={folder === undefined ? tr("New account folder") : tr("Rename folder")}
      okLabel={folder === undefined ? tr("Create") : tr("Rename")}
      onOk={() => void save()}
      onCancel={onClose}
      okDisabled={trimmed === "" || problem !== null || trimmed === folder}
      busy={busy}
      error={error}
    >
      <TextInput
        label={tr("Folder name")}
        value={name}
        onChange={(event) => setName(event.currentTarget.value)}
        error={problem}
        data-autofocus
      />
    </OkCancelDialog>
  );
}
