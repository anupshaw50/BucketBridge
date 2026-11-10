import { ActionIcon, Alert, Badge, Button, Checkbox, Group, Select, Stack, Switch, Table, Text, Textarea, TextInput, Tooltip } from "@mantine/core";
import { IconArrowDown, IconArrowUp, IconPlus, IconTrash } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { errorText } from "../../api/errors";
import type { UploadRule, UploadRuleMatch, UploadRuleSse, UploadRulesSettings } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { headerLines, newRule, parseHeaderLines, RULE_SSE_VALUES, RULE_STORAGE_CLASSES, ruleValues } from "../../lib/uploadRules";

const SSE_LABELS: Record<UploadRuleSse, string> = {
  AES256: tr("SSE-S3 (AES256)"),
  "aws:kms": tr("SSE-KMS"),
  "aws:kms:dsse": tr("DSSE-KMS (dual layer)"),
};

/** The text of an optional header field: empty means "leave the value alone". */
function orNull(text: string): string | null {
  return text.trim() === "" ? null : text.trim();
}

function ruleTitle(rule: UploadRule, index: number): string {
  return rule.name.trim() !== "" ? rule.name : rule.pattern.trim() !== "" ? rule.pattern : tr("Rule {number}", { number: index + 1 });
}

/** Result of testing a file name against the rules being edited. */
function TestResult({ result, error }: { result: UploadRuleMatch | null; error: string | null }) {
  if (error !== null) {
    return (
      <Text size="sm" c="red" role="alert">
        {error}
      </Text>
    );
  }
  if (result === null) {
    return null;
  }
  if (result.rule === null || result.index === null) {
    return (
      <Text size="sm" c="dimmed" aria-live="polite">
        {tr("No rule matches this name. The upload keeps the settings of the bucket and the type guessed from the extension.")}
      </Text>
    );
  }
  const values = ruleValues(result.rule);
  return (
    <Stack gap={2} aria-live="polite">
      <Text size="sm">{tr("Rule {number} matches: {pattern}", { number: result.index + 1, pattern: result.rule.pattern })}</Text>
      {values.length === 0 ? (
        <Text size="sm" c="dimmed">
          {tr("This rule sets no values.")}
        </Text>
      ) : (
        values.map(([label, value]) => (
          <Text size="sm" key={label} c="dimmed">
            {label}: {value}
          </Text>
        ))
      )}
    </Stack>
  );
}

/**
 * The Upload rules page of the settings: an ordered list of file patterns and the values an upload gets from
 * the first rule that matches (content type, headers, storage class, encryption), with a way to test a name.
 */
export function UploadRulesPage({
  value,
  onChange,
  error,
}: {
  value: UploadRulesSettings;
  onChange: (next: UploadRulesSettings) => void;
  error?: string;
}) {
  const model = useModel();
  const [selected, setSelected] = useState<number | null>(value.rules.length > 0 ? 0 : null);
  const [testName, setTestName] = useState("");
  const [result, setResult] = useState<UploadRuleMatch | null>(null);
  const [testError, setTestError] = useState<string | null>(null);
  const [headerText, setHeaderText] = useState<{ index: number; text: string } | null>(null);

  // A rule test follows the rules and the name as they are typed; the host applies the same matcher as an upload.
  useEffect(() => {
    if (testName.trim() === "") {
      return;
    }
    let current = true;
    const timer = setTimeout(() => {
      model.api
        .test_upload_rules({ rules: value, name: testName.trim() })
        .then((match) => {
          if (current) {
            setResult(match);
            setTestError(null);
          }
        })
        .catch((failure: unknown) => {
          if (current) {
            setResult(null);
            setTestError(errorText(failure));
          }
        });
    }, 150);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [model, value, testName]);

  const rules = value.rules;
  const rule = selected === null ? undefined : rules[selected];
  const setRules = (next: UploadRule[]) => onChange({ ...value, rules: next });
  const update = (index: number, patch: Partial<UploadRule>) =>
    setRules(rules.map((existing, at) => (at === index ? { ...existing, ...patch } : existing)));
  const move = (index: number, by: -1 | 1) => {
    const target = index + by;
    if (target < 0 || target >= rules.length) {
      return;
    }
    const next = [...rules];
    const moved = next[index];
    const other = next[target];
    if (moved === undefined || other === undefined) {
      return;
    }
    next[index] = other;
    next[target] = moved;
    setRules(next);
    setSelected(target);
  };
  const remove = (index: number) => {
    setRules(rules.filter((_, at) => at !== index));
    setSelected(rules.length <= 1 ? null : Math.min(index, rules.length - 2));
  };
  const add = () => {
    setRules([...rules, newRule()]);
    setSelected(rules.length);
  };

  return (
    <Stack gap="sm">
      <Text size="sm">
        {tr(
          "An upload takes its content type, headers, storage class and encryption from the first rule whose pattern matches the file name. Values chosen for one transfer win over a rule. Patterns without a slash match the file name, patterns with a slash match the whole key.",
        )}
      </Text>
      <Switch
        label={tr("Apply upload rules")}
        checked={value.enabled}
        onChange={(event) => onChange({ ...value, enabled: event.currentTarget.checked })}
      />
      {error !== undefined ? (
        <Alert color="red" role="alert" py="xs">
          {error}
        </Alert>
      ) : null}
      {rules.length === 0 ? (
        <Text size="sm" c="dimmed">
          {tr("No rules yet. Add a rule such as *.html to set a content type or a cache policy.")}
        </Text>
      ) : (
        <Table aria-label={tr("Upload rules")} verticalSpacing={4} fz="sm" highlightOnHover>
          <Table.Thead>
            <Table.Tr>
              <Table.Th w={40}>{tr("On")}</Table.Th>
              <Table.Th w={36}>#</Table.Th>
              <Table.Th>{tr("Pattern")}</Table.Th>
              <Table.Th>{tr("Sets")}</Table.Th>
              <Table.Th w={110} />
            </Table.Tr>
          </Table.Thead>
          <Table.Tbody>
            {rules.map((item, index) => (
              <Table.Tr
                key={index}
                bg={selected === index ? "var(--mantine-color-blue-light)" : undefined}
                onClick={() => setSelected(index)}
                style={{ cursor: "pointer" }}
              >
                <Table.Td onClick={(event) => event.stopPropagation()}>
                  <Checkbox
                    aria-label={tr("Rule {number} is on", { number: index + 1 })}
                    checked={item.enabled}
                    onChange={(event) => update(index, { enabled: event.currentTarget.checked })}
                  />
                </Table.Td>
                <Table.Td>{index + 1}</Table.Td>
                <Table.Td style={{ overflowWrap: "anywhere" }}>{ruleTitle(item, index)}</Table.Td>
                <Table.Td>
                  <Group gap={4}>
                    {ruleValues(item).map(([label]) => (
                      <Badge key={label} variant="light" size="sm" tt="none">
                        {label}
                      </Badge>
                    ))}
                  </Group>
                </Table.Td>
                <Table.Td onClick={(event) => event.stopPropagation()}>
                  <Group gap={2} wrap="nowrap" justify="flex-end">
                    <Tooltip label={tr("Move up")}>
                      <ActionIcon
                        variant="subtle"
                        aria-label={tr("Move rule {number} up", { number: index + 1 })}
                        disabled={index === 0}
                        onClick={() => move(index, -1)}
                      >
                        <IconArrowUp size={16} />
                      </ActionIcon>
                    </Tooltip>
                    <Tooltip label={tr("Move down")}>
                      <ActionIcon
                        variant="subtle"
                        aria-label={tr("Move rule {number} down", { number: index + 1 })}
                        disabled={index === rules.length - 1}
                        onClick={() => move(index, 1)}
                      >
                        <IconArrowDown size={16} />
                      </ActionIcon>
                    </Tooltip>
                    <Tooltip label={tr("Delete rule")}>
                      <ActionIcon
                        variant="subtle"
                        color="red"
                        aria-label={tr("Delete rule {number}", { number: index + 1 })}
                        onClick={() => remove(index)}
                      >
                        <IconTrash size={16} />
                      </ActionIcon>
                    </Tooltip>
                  </Group>
                </Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      )}
      <Group>
        <Button variant="default" leftSection={<IconPlus size={14} />} onClick={add}>
          {tr("Add rule")}
        </Button>
      </Group>
      {rule !== undefined && selected !== null ? (
        <Stack gap="xs" role="group" aria-label={tr("Rule {number}", { number: selected + 1 })}>
          <Text fw={600} size="sm">
            {tr("Rule {number}", { number: selected + 1 })}
          </Text>
          <Group align="flex-end" grow>
            <TextInput
              label={tr("Name (optional)")}
              value={rule.name}
              onChange={(event) => update(selected, { name: event.currentTarget.value })}
            />
            <TextInput
              label={tr("File pattern")}
              placeholder="*.html"
              value={rule.pattern}
              spellCheck={false}
              onChange={(event) => update(selected, { pattern: event.currentTarget.value })}
            />
          </Group>
          <Switch
            label={tr("Ignore case in the pattern")}
            checked={rule.case_insensitive}
            onChange={(event) => update(selected, { case_insensitive: event.currentTarget.checked })}
          />
          <TextInput
            label={tr("Content-Type")}
            placeholder="text/html; charset=utf-8"
            value={rule.content_type ?? ""}
            spellCheck={false}
            onChange={(event) => update(selected, { content_type: orNull(event.currentTarget.value) })}
          />
          <TextInput
            label={tr("Cache-Control")}
            placeholder="public, max-age=3600"
            value={rule.cache_control ?? ""}
            spellCheck={false}
            onChange={(event) => update(selected, { cache_control: orNull(event.currentTarget.value) })}
          />
          <Group grow align="flex-start">
            <TextInput
              label={tr("Content-Disposition")}
              placeholder="attachment"
              value={rule.content_disposition ?? ""}
              spellCheck={false}
              onChange={(event) => update(selected, { content_disposition: orNull(event.currentTarget.value) })}
            />
            <TextInput
              label={tr("Content-Encoding")}
              placeholder="gzip"
              value={rule.content_encoding ?? ""}
              spellCheck={false}
              onChange={(event) => update(selected, { content_encoding: orNull(event.currentTarget.value) })}
            />
          </Group>
          <Textarea
            label={tr("Other headers")}
            description={tr("One per line as Name: value. Content-Language, Expires and x-amz-meta-<name> are supported.")}
            value={headerText?.index === selected ? headerText.text : headerLines(rule.headers)}
            rows={3}
            spellCheck={false}
            error={
              headerText?.index === selected && parseHeaderLines(headerText.text).badLine !== null
                ? tr("Line {line} needs a name and a value separated by a colon.", {
                    line: parseHeaderLines(headerText.text).badLine ?? 0,
                  })
                : undefined
            }
            onChange={(event) => {
              const text = event.currentTarget.value;
              setHeaderText({ index: selected, text });
              update(selected, { headers: parseHeaderLines(text).headers });
            }}
            onBlur={() => setHeaderText(null)}
          />
          <Group grow align="flex-start">
            <Select
              label={tr("Storage class")}
              placeholder={tr("Bucket default")}
              clearable
              data={[...RULE_STORAGE_CLASSES]}
              value={rule.storage_class}
              onChange={(next) => update(selected, { storage_class: next })}
              comboboxProps={{ withinPortal: false }}
            />
            <Select
              label={tr("Server-side encryption")}
              placeholder={tr("Bucket default")}
              clearable
              data={RULE_SSE_VALUES.map((sse) => ({ value: sse, label: SSE_LABELS[sse] }))}
              value={rule.sse}
              onChange={(next) => {
                const sse = RULE_SSE_VALUES.find((known) => known === next) ?? null;
                update(selected, { sse, sse_kms_key_id: sse === "aws:kms" || sse === "aws:kms:dsse" ? rule.sse_kms_key_id : null });
              }}
              comboboxProps={{ withinPortal: false }}
            />
          </Group>
          {rule.sse === "aws:kms" || rule.sse === "aws:kms:dsse" ? (
            <TextInput
              label={tr("KMS key (ID, ARN or alias)")}
              placeholder={tr("The default key of the account")}
              value={rule.sse_kms_key_id ?? ""}
              spellCheck={false}
              onChange={(event) => update(selected, { sse_kms_key_id: orNull(event.currentTarget.value) })}
            />
          ) : null}
        </Stack>
      ) : null}
      <Stack gap={4}>
        <TextInput
          label={tr("Test a file name")}
          description={tr("Shows which rule an upload of this name would use.")}
          placeholder="photos/Holiday.JPG"
          value={testName}
          spellCheck={false}
          onChange={(event) => setTestName(event.currentTarget.value)}
        />
        <TestResult result={testName.trim() === "" ? null : result} error={testName.trim() === "" ? null : testError} />
      </Stack>
    </Stack>
  );
}
