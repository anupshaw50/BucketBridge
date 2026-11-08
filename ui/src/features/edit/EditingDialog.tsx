import { Badge, Button, Group, Stack, Table, Text } from "@mantine/core";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import type { EditState } from "../../api/types";
import { editStateLabel } from "../../lib/edit";

const STATE_COLOR: Record<EditState, string> = {
  watching: "gray",
  uploading: "blue",
  uploaded: "green",
  conflict: "orange",
  error: "red",
  stopped: "gray",
};

/** The objects being edited in place: their state, with Stop for each, and the way into a conflict. */
export function EditingDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const sessions = useSlice(model.edits.store, (state) => state.sessions);

  return (
    <AppModal opened onClose={onClose} title={tr("Editing")} size="lg">
      <Stack gap="sm">
        <Text size="sm" c="dimmed">
          {tr("Each save of the local copy is uploaded to the object. Stop editing to upload a pending save and remove the copy.")}
        </Text>
        {sessions.length === 0 ? (
          <Text size="sm">{tr("No objects are being edited.")}</Text>
        ) : (
          <Table aria-label={tr("Objects being edited")} verticalSpacing="xs">
            <Table.Tbody>
              {sessions.map((session) => (
                <Table.Tr key={session.id}>
                  <Table.Td style={{ overflowWrap: "anywhere" }}>
                    <Text size="sm">{session.key}</Text>
                    <Text size="xs" c="dimmed">
                      {session.bucket}
                    </Text>
                    {session.state === "error" && session.error !== null ? (
                      <Text size="xs" c="red">
                        {session.error}
                      </Text>
                    ) : null}
                  </Table.Td>
                  <Table.Td>
                    <Badge color={STATE_COLOR[session.state]} variant="light">
                      {editStateLabel(session.state)}
                    </Badge>
                  </Table.Td>
                  <Table.Td>
                    <Group gap="xs" justify="flex-end" wrap="nowrap">
                      {session.state === "conflict" ? (
                        <Button
                          size="compact-sm"
                          variant="default"
                          onClick={() => model.dialogs.open({ kind: "edit-conflict", sessionId: session.id })}
                        >
                          {tr("Resolve…")}
                        </Button>
                      ) : null}
                      {session.state === "error" ? (
                        <Button size="compact-sm" variant="default" onClick={() => void model.edits.resolve(session.id, "retry")}>
                          {tr("Retry")}
                        </Button>
                      ) : null}
                      <Button
                        size="compact-sm"
                        variant="default"
                        aria-label={tr("Stop editing {key}", { key: session.key })}
                        onClick={() => void model.edits.stop(session.id)}
                      >
                        {tr("Stop")}
                      </Button>
                    </Group>
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        )}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {tr("Close")}
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}
