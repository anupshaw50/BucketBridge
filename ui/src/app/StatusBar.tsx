import { CloseButton, Group, Text, Tooltip, UnstyledButton } from "@mantine/core";
import { IconAlertCircle, IconInfoCircle, IconLock, IconLockOpen } from "@tabler/icons-react";
import { formatCount, plural } from "../lib/format";
import { tr } from "../i18n";
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
    <footer className="bb-statusbar" aria-label={tr("Status bar")}>
      <Group gap={6} wrap="nowrap" style={{ minWidth: 0, flex: 1 }} role="status" aria-live="polite">
        {message !== null ? (
          <>
            {message.level === "error" ? (
              <IconAlertCircle size={14} color="var(--mantine-color-red-6)" aria-label={tr("Error")} />
            ) : (
              <IconInfoCircle size={14} color="var(--mantine-color-blue-6)" aria-label={tr("Information")} />
            )}
            <Text size="xs" className="bb-ellipsis" c={message.level === "error" ? "red" : undefined} title={message.text}>
              {message.text}
            </Text>
            <CloseButton size="xs" aria-label={tr("Dismiss message")} onClick={() => model.status.clear()} />
          </>
        ) : (
          <Text size="xs" c="dimmed">
            {tr("Ready")}
          </Text>
        )}
      </Group>
      <Group gap="md" wrap="nowrap">
        {bucket !== null ? (
          <Text size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }}>
            {plural(shown, "item")}
            {selected > 0 ? tr(", {n} selected", { n: formatCount(selected) }) : ""}
          </Text>
        ) : null}
        {counts.running + counts.queued > 0 ? (
          <Text size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }}>
            {tr("{running} running, {queued} queued", {
              running: formatCount(counts.running),
              queued: formatCount(counts.queued),
            })}
          </Text>
        ) : null}
        {edits.length > 0 ? (
          <UnstyledButton
            aria-label={`Editing ${plural(edits.length, "object")}`}
            title={tr("Objects being edited")}
            onClick={() => model.dialogs.open({ kind: "edit-sessions" })}
          >
            <Text size="xs" c={needAttention > 0 ? "red" : "dimmed"} style={{ whiteSpace: "nowrap" }}>
              {tr("Editing {n}", { n: formatCount(edits.length) })}
              {needAttention > 0 ? tr(", {n} need attention", { n: formatCount(needAttention) }) : ""}
            </Text>
          </UnstyledButton>
        ) : null}
        {counts.failed > 0 ? (
          <Text size="xs" c="red" style={{ whiteSpace: "nowrap" }}>
            {plural(counts.failed, "failed transfer")}
          </Text>
        ) : null}
        {security?.master_password_set ? (
          <Tooltip label={locked ? tr("Secrets are locked") : tr("Secrets are unlocked")}>
            {locked ? (
              <IconLock size={14} aria-label={tr("Secrets locked")} />
            ) : (
              <IconLockOpen size={14} aria-label={tr("Secrets unlocked")} />
            )}
          </Tooltip>
        ) : null}
        <Text size="xs" style={{ whiteSpace: "nowrap" }} aria-label={tr("Active account")}>
          {account ? `${account.label} (${account.region})` : tr("No account")}
        </Text>
      </Group>
    </footer>
  );
}
