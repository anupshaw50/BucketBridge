import { Button, Stack, Text } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { AppModal } from "../../components/AppModal";
import { applicationName } from "./helpers";

/** "Open with": downloads the object and opens the copy with a recent application or one the user picks. */
export function OpenWithDialog({
  accountId,
  bucket,
  objectKey,
  onClose,
}: {
  accountId: string;
  bucket: string;
  objectKey: string;
  onClose: () => void;
}) {
  const model = useModel();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const recent = model.settings.current.general.open_with_recent;

  const openWith = async (app: string) => {
    setBusy(true);
    setError(null);
    try {
      await model.api.open_object_with({ account_id: accountId, bucket, key: objectKey, app });
      await model.settings.load().catch(() => undefined);
      model.status.info(tr("Opening {key} with {app}.", { key: objectKey, app: applicationName(app) }));
      onClose();
    } catch (raw) {
      setError(toBbError(raw).message);
      setBusy(false);
    }
  };

  const choose = async () => {
    try {
      const app = await model.platform.pickApplication(tr("Choose an application"));
      if (app !== null) {
        await openWith(app);
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  return (
    <AppModal opened onClose={onClose} title={tr("Open with")} size="md" closeOnClickOutside={!busy}>
      <Stack gap="sm">
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {objectKey}
        </Text>
        {recent.length > 0 && (
          <Stack gap={4} aria-label={tr("Recent applications")} role="group">
            <Text size="xs" c="dimmed">
              {tr("Recent applications")}
            </Text>
            {recent.map((app) => (
              <Button key={app} variant="default" justify="flex-start" disabled={busy} onClick={() => void openWith(app)} title={app}>
                {applicationName(app)}
              </Button>
            ))}
          </Stack>
        )}
        {error !== null && (
          <Text size="sm" c="red" role="alert">
            {error}
          </Text>
        )}
        <Button loading={busy} onClick={() => void choose()}>
          {tr("Choose application…")}
        </Button>
        <Button variant="subtle" onClick={onClose} disabled={busy}>
          {tr("Cancel")}
        </Button>
      </Stack>
    </AppModal>
  );
}
