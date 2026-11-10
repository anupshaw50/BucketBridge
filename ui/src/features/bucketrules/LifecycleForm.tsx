import { ActionIcon, Alert, Button, Group, NumberInput, Paper, Select, Stack, Switch, Text, TextInput } from "@mantine/core";
import { IconPlus, IconTrash } from "@tabler/icons-react";
import type { LifecycleConfig, LifecycleRule } from "../../api/types";
import { tr } from "../../i18n";
import { emptyRule, parseRules, rulesText, TRANSITION_CLASSES } from "./lifecycleModel";

const asNumber = (value: number | string): number | null => (typeof value === "number" && Number.isFinite(value) ? value : null);
const asText = (value: string): string | null => (value.length === 0 ? null : value);

const CLASS_OPTIONS = TRANSITION_CLASSES.map((value) => ({ value, label: value }));

/** One row of a list with a button that removes it. */
function Row({ label, onRemove, children }: { label: string; onRemove: () => void; children: React.ReactNode }) {
  return (
    <Group gap="xs" align="flex-end" wrap="wrap">
      {children}
      <ActionIcon variant="subtle" color="red" aria-label={label} title={label} onClick={onRemove}>
        <IconTrash size={16} />
      </ActionIcon>
    </Group>
  );
}

function replaceAt<T>(list: T[], index: number, next: T): T[] {
  return list.map((entry, i) => (i === index ? next : entry));
}

function RuleCard({ rule, index, onChange, onRemove }: { rule: LifecycleRule; index: number; onChange: (rule: LifecycleRule) => void; onRemove: () => void }) {
  const set = (patch: Partial<LifecycleRule>) => onChange({ ...rule, ...patch });
  const setFilter = (patch: Partial<LifecycleRule["filter"]>) => set({ filter: { ...rule.filter, ...patch } });
  const expiration = rule.expiration;
  const setExpiration = (patch: Partial<NonNullable<LifecycleRule["expiration"]>>) => {
    const next = { days: null, date: null, expired_object_delete_marker: false, ...expiration, ...patch };
    set({ expiration: next.days === null && next.date === null && !next.expired_object_delete_marker ? null : next });
  };
  const noncurrent = rule.noncurrent_version_expiration;
  const setNoncurrent = (patch: Partial<NonNullable<LifecycleRule["noncurrent_version_expiration"]>>) => {
    const next = { noncurrent_days: null, newer_noncurrent_versions: null, ...noncurrent, ...patch };
    set({ noncurrent_version_expiration: next.noncurrent_days === null && next.newer_noncurrent_versions === null ? null : next });
  };
  return (
    <Paper withBorder p="sm" role="group" aria-label={tr("Rule {n}", { n: index + 1 })}>
      <Stack gap="sm">
        <Group justify="space-between" align="flex-end">
          <Text fw={600} size="sm">
            {tr("Rule {n}", { n: index + 1 })}
          </Text>
          <Button size="compact-xs" variant="light" color="red" leftSection={<IconTrash size={14} />} onClick={onRemove}>
            {tr("Remove rule")}
          </Button>
        </Group>
        <Group align="flex-end" grow>
          <TextInput label={tr("Rule ID")} value={rule.id ?? ""} onChange={(event) => set({ id: asText(event.currentTarget.value) })} />
          <Select
            label={tr("Status")}
            allowDeselect={false}
            value={rule.status}
            onChange={(value) => set({ status: value === "disabled" ? "disabled" : "enabled" })}
            data={[
              { value: "enabled", label: tr("Enabled") },
              { value: "disabled", label: tr("Disabled") },
            ]}
          />
        </Group>

        <Text size="sm" fw={500}>
          {tr("Filter")}
        </Text>
        <Group align="flex-end" grow>
          <TextInput label={tr("Prefix")} value={rule.filter.prefix ?? ""} onChange={(event) => setFilter({ prefix: asText(event.currentTarget.value) })} />
          <NumberInput
            label={tr("Larger than (bytes)")}
            min={0}
            allowDecimal={false}
            value={rule.filter.object_size_greater_than ?? ""}
            onChange={(value) => setFilter({ object_size_greater_than: asNumber(value) })}
          />
          <NumberInput
            label={tr("Smaller than (bytes)")}
            min={0}
            allowDecimal={false}
            value={rule.filter.object_size_less_than ?? ""}
            onChange={(value) => setFilter({ object_size_less_than: asNumber(value) })}
          />
        </Group>
        {rule.filter.tags.map((tag, i) => (
          <Row key={i} label={tr("Remove tag")} onRemove={() => setFilter({ tags: rule.filter.tags.filter((_, n) => n !== i) })}>
            <TextInput label={tr("Tag key")} value={tag.key} onChange={(event) => setFilter({ tags: replaceAt(rule.filter.tags, i, { ...tag, key: event.currentTarget.value }) })} />
            <TextInput label={tr("Tag value")} value={tag.value} onChange={(event) => setFilter({ tags: replaceAt(rule.filter.tags, i, { ...tag, value: event.currentTarget.value }) })} />
          </Row>
        ))}
        <Group>
          <Button size="compact-xs" variant="default" leftSection={<IconPlus size={14} />} onClick={() => setFilter({ tags: [...rule.filter.tags, { key: "", value: "" }] })}>
            {tr("Add tag")}
          </Button>
        </Group>

        <Text size="sm" fw={500}>
          {tr("Transitions of current versions")}
        </Text>
        {rule.transitions.map((transition, i) => (
          <Row key={i} label={tr("Remove transition")} onRemove={() => set({ transitions: rule.transitions.filter((_, n) => n !== i) })}>
            <NumberInput
              label={tr("After (days)")}
              min={0}
              allowDecimal={false}
              value={transition.days ?? ""}
              onChange={(value) => set({ transitions: replaceAt(rule.transitions, i, { ...transition, days: asNumber(value) }) })}
            />
            <TextInput
              label={tr("On date (YYYY-MM-DD)")}
              value={transition.date ?? ""}
              onChange={(event) => set({ transitions: replaceAt(rule.transitions, i, { ...transition, date: asText(event.currentTarget.value) }) })}
            />
            <Select
              label={tr("Storage class")}
              allowDeselect={false}
              value={transition.storage_class || null}
              data={CLASS_OPTIONS}
              onChange={(value) => set({ transitions: replaceAt(rule.transitions, i, { ...transition, storage_class: value ?? "" }) })}
            />
          </Row>
        ))}
        <Group>
          <Button
            size="compact-xs"
            variant="default"
            leftSection={<IconPlus size={14} />}
            onClick={() => set({ transitions: [...rule.transitions, { days: 30, date: null, storage_class: "STANDARD_IA" }] })}
          >
            {tr("Add transition")}
          </Button>
        </Group>

        <Text size="sm" fw={500}>
          {tr("Expiration of current versions")}
        </Text>
        <Group align="flex-end">
          <NumberInput label={tr("After (days)")} min={0} allowDecimal={false} value={expiration?.days ?? ""} onChange={(value) => setExpiration({ days: asNumber(value) })} />
          <TextInput label={tr("On date (YYYY-MM-DD)")} value={expiration?.date ?? ""} onChange={(event) => setExpiration({ date: asText(event.currentTarget.value) })} />
          <Switch
            label={tr("Remove expired delete markers")}
            checked={expiration?.expired_object_delete_marker ?? false}
            onChange={(event) => setExpiration({ expired_object_delete_marker: event.currentTarget.checked })}
            pb={6}
          />
        </Group>

        <Text size="sm" fw={500}>
          {tr("Noncurrent versions")}
        </Text>
        {rule.noncurrent_version_transitions.map((transition, i) => (
          <Row
            key={i}
            label={tr("Remove noncurrent transition")}
            onRemove={() => set({ noncurrent_version_transitions: rule.noncurrent_version_transitions.filter((_, n) => n !== i) })}
          >
            <NumberInput
              label={tr("Noncurrent for (days)")}
              min={0}
              allowDecimal={false}
              value={transition.noncurrent_days}
              onChange={(value) => set({ noncurrent_version_transitions: replaceAt(rule.noncurrent_version_transitions, i, { ...transition, noncurrent_days: asNumber(value) ?? 0 }) })}
            />
            <NumberInput
              label={tr("Newer versions to keep")}
              min={0}
              allowDecimal={false}
              value={transition.newer_noncurrent_versions ?? ""}
              onChange={(value) => set({ noncurrent_version_transitions: replaceAt(rule.noncurrent_version_transitions, i, { ...transition, newer_noncurrent_versions: asNumber(value) }) })}
            />
            <Select
              label={tr("Storage class")}
              allowDeselect={false}
              value={transition.storage_class || null}
              data={CLASS_OPTIONS}
              onChange={(value) => set({ noncurrent_version_transitions: replaceAt(rule.noncurrent_version_transitions, i, { ...transition, storage_class: value ?? "" }) })}
            />
          </Row>
        ))}
        <Group>
          <Button
            size="compact-xs"
            variant="default"
            leftSection={<IconPlus size={14} />}
            onClick={() =>
              set({ noncurrent_version_transitions: [...rule.noncurrent_version_transitions, { noncurrent_days: 30, newer_noncurrent_versions: null, storage_class: "STANDARD_IA" }] })
            }
          >
            {tr("Add noncurrent transition")}
          </Button>
        </Group>
        <Group align="flex-end">
          <NumberInput
            label={tr("Delete noncurrent versions after (days)")}
            min={0}
            allowDecimal={false}
            value={noncurrent?.noncurrent_days ?? ""}
            onChange={(value) => setNoncurrent({ noncurrent_days: asNumber(value) })}
          />
          <NumberInput
            label={tr("Newer versions to keep")}
            aria-label={tr("Newer versions to keep when deleting")}
            min={0}
            allowDecimal={false}
            value={noncurrent?.newer_noncurrent_versions ?? ""}
            onChange={(value) => setNoncurrent({ newer_noncurrent_versions: asNumber(value) })}
          />
        </Group>

        <NumberInput
          label={tr("Abort incomplete multipart uploads after (days)")}
          min={1}
          allowDecimal={false}
          value={rule.abort_incomplete_multipart_upload_days ?? ""}
          onChange={(value) => set({ abort_incomplete_multipart_upload_days: asNumber(value) })}
        />
      </Stack>
    </Paper>
  );
}

/**
 * The form view of the lifecycle rules. It reads the JSON text of the editor and writes it back on every change, so
 * the form and the JSON always show the same document.
 */
export function LifecycleForm({ text, setText }: { text: string; setText: (text: string) => void }) {
  const parsed = parseRules(text);
  if (!parsed.ok) {
    return (
      <Alert color="yellow" role="alert">
        {tr("The JSON cannot be shown as a form until it is fixed: {message}", { message: parsed.message })}
      </Alert>
    );
  }
  const config = parsed.config;
  const write = (rules: LifecycleRule[]) => setText(rulesText({ ...config, rules } satisfies LifecycleConfig));
  return (
    <Stack gap="sm" data-testid="lifecycle-form">
      {config.rules.length === 0 ? (
        <Text size="sm" c="dimmed">
          {tr("There are no rules yet. Add one.")}
        </Text>
      ) : null}
      {config.rules.map((rule, index) => (
        <RuleCard
          key={index}
          rule={rule}
          index={index}
          onChange={(next) => write(replaceAt(config.rules, index, next))}
          onRemove={() => write(config.rules.filter((_, n) => n !== index))}
        />
      ))}
      <Group>
        <Button variant="default" leftSection={<IconPlus size={16} />} onClick={() => write([...config.rules, emptyRule()])}>
          {tr("Add rule")}
        </Button>
      </Group>
    </Stack>
  );
}
