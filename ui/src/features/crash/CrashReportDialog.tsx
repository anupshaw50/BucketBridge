import { Button, Code, Group, ScrollArea, Stack, Text } from "@mantine/core";
import { IconCopy, IconFolderOpen } from "@tabler/icons-react";
import { useState } from "react";
import { errorText } from "../../api/errors";
import type { CrashReport } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { AppModal } from "../../components/AppModal";
import { CopyField } from "../../components/CopyField";
import { formatDateTime } from "../../lib/format";

/** Shown at startup when the previous run crashed. Nothing is uploaded anywhere. */
export function CrashReportDialog({ report, onClose }: { report: CrashReport; onClose: () => void }) {
  const model = useModel();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const copy = async () => {
    try {
      await model.platform.copyText(report.contents);
      model.status.info(tr("Crash report copied to the clipboard."));
    } catch (failure) {
      setError(errorText(failure));
    }
  };
  const reveal = async () => {
    try {
      await model.platform.revealPath(report.path);
    } catch (failure) {
      setError(errorText(failure));
    }
  };
  const dismiss = async () => {
    setBusy(true);
    setError(null);
    try {
      await model.api.dismiss_crash_report({ path: report.path });
      onClose();
    } catch (failure) {
      setError(errorText(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <AppModal opened onClose={onClose} title={tr("BucketBridge closed unexpectedly")} size="xl">
      <Stack gap="sm">
        <Text size="sm">
          {tr("The previous session ended with a crash on {date}.", { date: formatDateTime(report.created_at) })}{" "}
          {tr("A report was saved on this computer. It has not been sent anywhere; if you want to report the problem, copy it into an issue yourself.")}
        </Text>
        <CopyField label={tr("Report file")} value={report.path} />
        <ScrollArea h={220} type="auto">
          <Code block fz="xs" aria-label={tr("Crash report contents")}>
            {report.contents}
          </Code>
        </ScrollArea>
        {error !== null ? (
          <Text size="xs" c="red" role="alert">
            {error}
          </Text>
        ) : null}
        <Group justify="space-between">
          <Group gap="xs">
            <Button variant="default" leftSection={<IconCopy size={14} />} onClick={() => void copy()}>
              {tr("Copy report")}
            </Button>
            <Button variant="default" leftSection={<IconFolderOpen size={14} />} onClick={() => void reveal()}>
              {tr("Show in folder")}
            </Button>
          </Group>
          <Group gap="xs">
            <Button variant="default" onClick={onClose}>
              {tr("Remind me next time")}
            </Button>
            <Button loading={busy} onClick={() => void dismiss()}>
              {tr("Dismiss report")}
            </Button>
          </Group>
        </Group>
      </Stack>
    </AppModal>
  );
}
