import { ActionIcon, Alert, Button, Group, PasswordInput, Stack, Table, Text, TextInput } from "@mantine/core";
import { IconAlertCircle, IconCheck, IconInfoCircle, IconPlus, IconTrash } from "@tabler/icons-react";
import type { Row } from "../../lib/objectMeta";
import { baseName, displaySegment } from "../../lib/s3keys";
import { type Applier, type TabContext, sseKeyProblem, summarize } from "./apply";

/** The outcome of the last change: one message for one object, a table for several. */
export function ResultList({ applier, batch }: { applier: Applier; batch: boolean }) {
  const { results, error } = applier;
  if (error !== null) {
    return (
      <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" py="xs">
        {error}
      </Alert>
    );
  }
  if (results === null) {
    return null;
  }
  const only = results[0];
  if (!batch && results.length === 1 && only !== undefined) {
    if (only.error !== null) {
      return (
        <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" py="xs">
          {only.error.message}
        </Alert>
      );
    }
    return (
      <Alert
        color={only.warnings.length > 0 ? "yellow" : only.changed ? "teal" : "blue"}
        icon={only.changed ? <IconCheck size={16} /> : <IconInfoCircle size={16} />}
        role="status"
        py="xs"
      >
        <Text size="sm">{only.changed ? "Changed." : "Already up to date; nothing was changed."}</Text>
        {only.warnings.map((warning) => (
          <Text key={warning} size="xs" style={{ overflowWrap: "anywhere" }}>
            {warning}
          </Text>
        ))}
      </Alert>
    );
  }
  return (
    <Stack gap={4} role="status" aria-label="Results">
      <Text size="sm" fw={500}>
        {summarize(results)}
      </Text>
      <Table withTableBorder verticalSpacing={2} fz="xs">
        <Table.Thead>
          <Table.Tr>
            <Table.Th>Object</Table.Th>
            <Table.Th>Result</Table.Th>
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {results.map((result) => (
            <Table.Tr key={result.key}>
              <Table.Td style={{ overflowWrap: "anywhere" }}>{displaySegment(baseName(result.key))}</Table.Td>
              <Table.Td style={{ overflowWrap: "anywhere" }}>
                {result.error !== null ? (
                  <Text size="xs" c="red">
                    Failed: {result.error.message}
                  </Text>
                ) : (
                  <>
                    {result.changed ? (result.previous !== null ? `Changed (was ${result.previous})` : "Changed") : "Unchanged"}
                    {result.warnings.map((warning) => (
                      <Text key={warning} size="xs" c="orange">
                        {warning}
                      </Text>
                    ))}
                  </>
                )}
              </Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
    </Stack>
  );
}

export function ApplyBar({
  applier,
  batch,
  disabled,
  onApply,
  label = "Apply",
}: {
  applier: Applier;
  batch: boolean;
  disabled: boolean;
  onApply: () => void;
  label?: string;
}) {
  return (
    <Stack gap="xs">
      <ResultList applier={applier} batch={batch} />
      <Group justify="flex-end">
        <Button onClick={onApply} disabled={disabled} loading={applier.busy}>
          {label}
        </Button>
      </Group>
    </Stack>
  );
}

/** Rows of two text fields, with a button to add one and one to remove each. */
export function KeyValueEditor({
  rows,
  onChange,
  keyLabel,
  valueLabel,
  addLabel,
}: {
  rows: Row[];
  onChange: (rows: Row[]) => void;
  keyLabel: string;
  valueLabel: string;
  addLabel: string;
}) {
  const update = (index: number, patch: Partial<Row>) =>
    onChange(rows.map((row, at) => (at === index ? { ...row, ...patch } : row)));
  return (
    <Stack gap={6}>
      {rows.map((row, index) => (
        <Group key={index} gap="xs" wrap="nowrap" align="flex-end">
          <TextInput
            aria-label={`${keyLabel} ${index + 1}`}
            placeholder={keyLabel}
            value={row.key}
            onChange={(event) => update(index, { key: event.currentTarget.value })}
            autoComplete="off"
            spellCheck={false}
            style={{ flex: 1 }}
          />
          <TextInput
            aria-label={`${valueLabel} ${index + 1}`}
            placeholder={valueLabel}
            value={row.value}
            onChange={(event) => update(index, { value: event.currentTarget.value })}
            autoComplete="off"
            spellCheck={false}
            style={{ flex: 1 }}
          />
          <ActionIcon
            variant="subtle"
            color="red"
            aria-label={`Remove ${keyLabel.toLowerCase()} ${index + 1}`}
            onClick={() => onChange(rows.filter((_, at) => at !== index))}
          >
            <IconTrash size={16} />
          </ActionIcon>
        </Group>
      ))}
      <Group>
        <Button size="xs" variant="default" leftSection={<IconPlus size={14} />} onClick={() => onChange([...rows, { key: "", value: "" }])}>
          {addLabel}
        </Button>
      </Group>
    </Stack>
  );
}

/** The key of SSE-C objects, for requests that read or copy them. */
export function CurrentKeyField({ ctx }: { ctx: TabContext }) {
  const encrypted = ctx.details?.meta.sse.type === "c";
  if (!ctx.batch && !encrypted) {
    return null;
  }
  return (
    <PasswordInput
      label="Key of objects encrypted with SSE-C"
      description="Only for objects that are encrypted with a customer-provided key. It is sent with the request and never stored."
      value={ctx.sseKey}
      onChange={(event) => ctx.setSseKey(event.currentTarget.value)}
      error={sseKeyProblem(ctx)}
      autoComplete="off"
    />
  );
}

