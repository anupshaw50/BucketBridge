import { Alert, Button, Group, Stack, type ModalProps } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import type { FormEvent, ReactNode } from "react";
import { tr } from "../i18n";
import { AppModal } from "./AppModal";

export interface OkCancelDialogProps {
  opened: boolean;
  title: ReactNode;
  children: ReactNode;
  onOk: () => void;
  onCancel: () => void;
  okLabel?: string;
  cancelLabel?: string;
  okDisabled?: boolean;
  /** Shows a spinner on OK and prevents a second submission. */
  busy?: boolean;
  danger?: boolean;
  /** Error from the last attempt, shown above the buttons. */
  error?: string | null;
  size?: ModalProps["size"];
  /** Extra buttons on the left of the footer. */
  extraActions?: ReactNode;
}

/** Standard dialog with OK and Cancel; Enter submits, Escape cancels. */
export function OkCancelDialog({
  opened,
  title,
  children,
  onOk,
  onCancel,
  okLabel = tr("OK"),
  cancelLabel = tr("Cancel"),
  okDisabled = false,
  busy = false,
  danger = false,
  error = null,
  size = "md",
  extraActions,
}: OkCancelDialogProps) {
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!okDisabled && !busy) {
      onOk();
    }
  };
  return (
    <AppModal opened={opened} onClose={onCancel} title={title} size={size} closeOnClickOutside={!busy}>
      <form onSubmit={submit} noValidate>
        <Stack gap="sm">
          {children}
          {error ? (
            <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" py="xs">
              {error}
            </Alert>
          ) : null}
          <Group justify="space-between" mt="xs">
            <Group gap="xs">{extraActions}</Group>
            <Group gap="xs">
              <Button variant="default" onClick={onCancel}>
                {cancelLabel}
              </Button>
              <Button type="submit" color={danger ? "red" : undefined} disabled={okDisabled} loading={busy}>
                {okLabel}
              </Button>
            </Group>
          </Group>
        </Stack>
      </form>
    </AppModal>
  );
}
