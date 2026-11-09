import { Button, Group, Stack, Text } from "@mantine/core";
import { useState } from "react";
import { errorText } from "../../api/errors";
import type { QuitMode, QuitRequest } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";

/** What is still active, one sentence per kind of work. */
function work(request: QuitRequest): string[] {
  const parts: string[] = [];
  if (request.active_transfers === 1) {
    parts.push(tr("1 transfer is running or waiting."));
  } else if (request.active_transfers > 1) {
    parts.push(tr("{n} transfers are running or waiting.", { n: request.active_transfers }));
  }
  if (request.active_syncs === 1) {
    parts.push(tr("1 sync run is in progress."));
  } else if (request.active_syncs > 1) {
    parts.push(tr("{n} sync runs are in progress.", { n: request.active_syncs }));
  }
  return parts;
}

/** Asked when BucketBridge is quit (window close, tray, Cmd-Q) while transfers or a sync run are active. */
export function QuitDialog({ request, onClose }: { request: QuitRequest; onClose: () => void }) {
  const model = useModel();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const quit = async (mode: QuitMode) => {
    setBusy(true);
    setError(null);
    try {
      await model.api.quit_app({ mode });
      onClose();
    } catch (failure) {
      setError(errorText(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <AppModal opened onClose={onClose} title={tr("Quit BucketBridge?")} size="md" closeOnClickOutside={false}>
      <Stack gap="sm">
        {work(request).map((line) => (
          <Text key={line} size="sm">
            {line}
          </Text>
        ))}
        <Text size="sm" c="dimmed">
          {tr("Quitting now stops running transfers; they stay in the queue and can be resumed. Running sync runs are cancelled.")}
        </Text>
        {error !== null ? (
          <Text size="xs" c="red" role="alert">
            {error}
          </Text>
        ) : null}
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onClose} disabled={busy}>
            {tr("Cancel")}
          </Button>
          <Button color="red" variant="light" onClick={() => void quit("now")} disabled={busy}>
            {tr("Quit now")}
          </Button>
          <Button onClick={() => void quit("when_idle")} disabled={busy}>
            {tr("Finish running tasks, then quit")}
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
