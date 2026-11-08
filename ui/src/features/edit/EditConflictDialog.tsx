import { Button, Group, Stack, Text } from "@mantine/core";
import { useEffect, useState } from "react";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import type { EditResolution } from "../../api/types";
import { copyKey } from "../../lib/edit";

/**
 * Opens when a save could not be uploaded because the object changed on the server (or was deleted) since it was
 * opened. Nothing is uploaded until the user chooses: overwrite the object, save the local version as a copy
 * under a new key, or discard the local changes. "Decide later" keeps the session; saves stay held.
 */
export function EditConflictDialog({ sessionId, onClose }: { sessionId: string; onClose: () => void }) {
  const model = useModel();
  const session = useSlice(model.edits.store, (state) => state.sessions.find((item) => item.id === sessionId));
  const [busy, setBusy] = useState(false);
  const conflicted = session?.state === "conflict";

  // The session ended (stopped elsewhere) or the conflict was settled: nothing left to decide.
  useEffect(() => {
    if (session === undefined || !conflicted) {
      onClose();
    }
  }, [session, conflicted, onClose]);

  if (session === undefined) {
    return null;
  }
  const deleted = session.conflict?.remote_deleted === true;
  const name = `s3://${session.bucket}/${session.key}`;

  const choose = async (resolution: EditResolution) => {
    if (resolution === "overwrite") {
      const confirmed = await model.dialogs.confirm({
        title: tr("Overwrite the object"),
        message: deleted
          ? tr("{name} was deleted. Upload your version and create it again?", { name })
          : tr("Replace the current {name} with your version? The change made on the server is lost.", { name }),
        confirmLabel: tr("Overwrite"),
        danger: true,
      });
      if (!confirmed) {
        return;
      }
    }
    if (resolution === "discard") {
      const confirmed = await model.dialogs.confirm({
        title: tr("Discard your changes"),
        message: tr("Drop your saved changes to {name} and stop editing? The local copy is deleted.", { name }),
        confirmLabel: tr("Discard"),
        danger: true,
      });
      if (!confirmed) {
        return;
      }
    }
    setBusy(true);
    const resolved = await model.edits.resolve(session.id, resolution);
    setBusy(false);
    if (resolved === null) {
      return;
    }
    if (resolution === "save_copy" && resolved.copy_key !== null) {
      model.status.info(tr("Saved your version as {key}.", { key: resolved.copy_key }));
    } else if (resolution === "overwrite") {
      model.status.info(tr("Uploaded your version of {key}.", { key: session.key }));
    } else if (resolution === "discard") {
      model.status.info(tr("Discarded your changes to {key}.", { key: session.key }));
    }
    onClose();
  };

  return (
    <AppModal opened onClose={onClose} title={tr("Edit conflict")} size="md" closeOnClickOutside={!busy}>
      <Stack gap="sm">
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {deleted
            ? tr("{name} was deleted on the server after you opened it.", { name })
            : tr("{name} was changed on the server after you opened it.", { name })}{" "}
          {tr("Your last save was not uploaded.")}
        </Text>
        <Stack gap="xs">
          <Button variant="default" disabled={busy} onClick={() => void choose("save_copy")}>
            {tr("Save as a copy")}
          </Button>
          <Text size="xs" c="dimmed" style={{ overflowWrap: "anywhere" }}>
            {tr("Uploads your version as {key} and stops editing. The object stays as it is.", { key: copyKey(session.key) })}
          </Text>
          <Button variant="default" color="red" disabled={busy} onClick={() => void choose("overwrite")}>
            {tr("Overwrite")}
          </Button>
          <Text size="xs" c="dimmed">
            {deleted ? tr("Creates the object again from your version.") : tr("Replaces the object on the server with your version.")}
          </Text>
          <Button variant="default" disabled={busy} onClick={() => void choose("discard")}>
            {tr("Discard my changes")}
          </Button>
        </Stack>
        <Group justify="flex-end">
          <Button variant="subtle" disabled={busy} onClick={onClose}>
            {tr("Decide later")}
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
