import { CloseButton, Group, Text, Tooltip, UnstyledButton } from "@mantine/core";
import { IconAlertCircle, IconInfoCircle, IconLock, IconLockOpen } from "@tabler/icons-react";
import { formatCount, plural } from "../lib/format";
import { useModel, useSlice } from "./context";

/** Bottom line: last error or information, selection, transfers and the active account. */
export function StatusBar() {
  const model = useModel();
  const message = useSlice(model.status.store, (state) => state.message);
  const selected = useSlice(model.browser.store, (state) => state.selection.keys.size);
  const shown = useSlice(model.browser.store, (state) => state.visible.length);
  const bucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const counts = useSlice(model.transfers.store, (state) => state.counts);
  const edits = useSlice(model.edits.store, (state) => state.sessions);
  const needAttention = edits.filter((session) => session.state === "conflict" || session.state === "error").length;
  const security = useSlice(model.session.store, (state) => state.security);
  const account = useSlice(model.session.store, (state) =>
    state.accounts.find((item) => item.id === state.activeAccountId),
  );
  const locked = security !== null && security.master_password_set && !security.unlocked;
  return (
    <footer className="bb-statusbar" aria-label="Status bar">
      <Group gap={6} wrap="nowrap" style={{ minWidth: 0, flex: 1 }} role="status" aria-live="polite">
        {message !== null ? (
          <>
            {message.level === "error" ? (
              <IconAlertCircle size={14} color="var(--mantine-color-red-6)" aria-label="Error" />
            ) : (
              <IconInfoCircle size={14} color="var(--mantine-color-blue-6)" aria-label="Information" />
            )}
            <Text size="xs" className="bb-ellipsis" c={message.level === "error" ? "red" : undefined} title={message.text}>
              {message.text}
            </Text>
            <CloseButton size="xs" aria-label="Dismiss message" onClick={() => model.status.clear()} />
          </>
        ) : (
          <Text size="xs" c="dimmed">
            Ready
          </Text>
        )}
      </Group>
      <Group gap="md" wrap="nowrap">
        {bucket !== null ? (
          <Text size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }}>
            {plural(shown, "item")}
            {selected > 0 ? `, ${formatCount(selected)} selected` : ""}
          </Text>
        ) : null}
        {counts.running + counts.queued > 0 ? (
          <Text size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }}>
            {formatCount(counts.running)} running, {formatCount(counts.queued)} queued
          </Text>
        ) : null}
        {edits.length > 0 ? (
          <UnstyledButton
            aria-label={`Editing ${plural(edits.length, "object")}`}
            title="Objects being edited"
            onClick={() => model.dialogs.open({ kind: "edit-sessions" })}
          >
            <Text size="xs" c={needAttention > 0 ? "red" : "dimmed"} style={{ whiteSpace: "nowrap" }}>
              Editing {formatCount(edits.length)}
              {needAttention > 0 ? `, ${formatCount(needAttention)} need attention` : ""}
            </Text>
          </UnstyledButton>
        ) : null}
        {counts.failed > 0 ? (
          <Text size="xs" c="red" style={{ whiteSpace: "nowrap" }}>
            {plural(counts.failed, "failed transfer")}
          </Text>
        ) : null}
        {security?.master_password_set ? (
          <Tooltip label={locked ? "Secrets are locked" : "Secrets are unlocked"}>
            {locked ? (
              <IconLock size={14} aria-label="Secrets locked" />
            ) : (
              <IconLockOpen size={14} aria-label="Secrets unlocked" />
            )}
          </Tooltip>
        ) : null}
        <Text size="xs" style={{ whiteSpace: "nowrap" }} aria-label="Active account">
          {account ? `${account.label} (${account.region})` : "No account"}
        </Text>
      </Group>
    </footer>
  );
}
