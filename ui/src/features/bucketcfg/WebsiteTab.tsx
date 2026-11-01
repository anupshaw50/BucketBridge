import { Alert, Button, Group, List, Paper, SegmentedControl, Select, SimpleGrid, Stack, Text, TextInput } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import { useState } from "react";
import type { WebsiteConfig } from "../../api/types";
import { useModel } from "../../app/context";
import { MAX_ROUTING_RULES, websiteProblems } from "../../lib/website";
import { ActionError, SectionGate, type TabProps } from "./common";
import { useAction } from "./useAction";
import {
  DEFAULT_INDEX_DOCUMENT,
  draftOf,
  newRule,
  type ProtocolChoice,
  type RuleDraft,
  sameWebsite,
  type WebsiteDraft,
  type WebsiteMode,
  websiteOf,
} from "./websiteDraft";

const PROTOCOLS: { value: ProtocolChoice; label: string }[] = [
  { value: "same", label: "Same as the request" },
  { value: "http", label: "http" },
  { value: "https", label: "https" },
];

function isProtocol(value: string | null): value is ProtocolChoice {
  return value === "same" || value === "http" || value === "https";
}

function isMode(value: string): value is WebsiteMode {
  return value === "off" || value === "hosting" || value === "redirect";
}

function ProtocolSelect({
  label,
  value,
  onChange,
}: {
  label: string;
  value: ProtocolChoice;
  onChange: (value: ProtocolChoice) => void;
}) {
  return (
    <Select
      label={label}
      data={PROTOCOLS}
      value={value}
      allowDeselect={false}
      comboboxProps={{ withinPortal: false }}
      onChange={(next) => {
        if (isProtocol(next)) {
          onChange(next);
        }
      }}
    />
  );
}

function RuleEditor({
  number,
  rule,
  onChange,
  onRemove,
}: {
  number: number;
  rule: RuleDraft;
  onChange: (next: RuleDraft) => void;
  onRemove: () => void;
}) {
  const text = (key: "keyPrefix" | "errorCode" | "host" | "replacePrefix" | "replaceKey" | "redirectCode") => ({
    value: rule[key],
    onChange: (event: { currentTarget: { value: string } }) => onChange({ ...rule, [key]: event.currentTarget.value }),
    spellCheck: false,
    autoComplete: "off",
  });
  return (
    <Paper withBorder p="sm" role="group" aria-label={`Routing rule ${number}`}>
      <Stack gap="xs">
        <Group justify="space-between">
          <Text size="sm" fw={600}>
            Routing rule {number}
          </Text>
          <Button size="compact-xs" variant="subtle" color="red" onClick={onRemove}>
            Remove rule
          </Button>
        </Group>
        <SimpleGrid cols={2} spacing="xs">
          <TextInput label="If the key starts with" placeholder="docs/" {...text("keyPrefix")} />
          <TextInput label="If the error code is" placeholder="404" {...text("errorCode")} />
          <ProtocolSelect
            label="Redirect protocol"
            value={rule.protocol}
            onChange={(protocol) => onChange({ ...rule, protocol })}
          />
          <TextInput label="Redirect to host" placeholder="www.example.com" {...text("host")} />
          <TextInput label="Replace the key prefix with" placeholder="documents/" {...text("replacePrefix")} />
          <TextInput label="Replace the whole key with" placeholder="error-page.html" {...text("replaceKey")} />
          <TextInput label="Redirect code" placeholder="301" {...text("redirectCode")} />
        </SimpleGrid>
      </Stack>
    </Paper>
  );
}

function WebsiteEditor({
  accountId,
  bucket,
  website,
  reload,
}: {
  accountId: string;
  bucket: string;
  website: WebsiteConfig | null;
  reload: () => Promise<void>;
}) {
  const model = useModel();
  const [draft, setDraft] = useState<WebsiteDraft>(() => draftOf(website));
  const [attempted, setAttempted] = useState(false);
  const action = useAction();

  const wanted = websiteOf(draft);
  const problems = wanted === null ? [] : websiteProblems(wanted);
  const change = (patch: Partial<WebsiteDraft>) => setDraft((current) => ({ ...current, ...patch }));
  const setMode = (mode: WebsiteMode) => {
    setAttempted(false);
    // Hosting always has an index document: start from the usual name.
    setDraft((current) => ({
      ...current,
      mode,
      index: current.index.trim().length === 0 ? DEFAULT_INDEX_DOCUMENT : current.index,
    }));
  };

  const save = () =>
    action.run(async () => {
      if (wanted === null) {
        return;
      }
      setAttempted(true);
      if (problems.length > 0) {
        return;
      }
      await model.api.put_bucket_website({ account_id: accountId, bucket, website: wanted });
      model.status.info(`The website configuration of ${bucket} was saved.`);
      await reload();
    });
  const disable = async () => {
    const confirmed = await model.dialogs.confirm({
      title: "Disable website hosting",
      message: `Website hosting of ${bucket} is turned off. Its website endpoint stops serving pages, and the routing rules are deleted.\n\nThe objects in the bucket are not changed.`,
      confirmLabel: "Disable hosting",
      danger: true,
    });
    if (!confirmed) {
      return;
    }
    await action.run(async () => {
      await model.api.delete_bucket_website({ account_id: accountId, bucket });
      model.status.info(`Website hosting of ${bucket} was turned off.`);
      await reload();
    });
  };

  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {website === null
          ? "This bucket does not serve a website."
          : website.type === "hosting"
            ? "This bucket serves a static website."
            : "This bucket redirects every request to another host."}
      </Text>
      <SegmentedControl<WebsiteMode>
        aria-label="Website mode"
        value={draft.mode}
        onChange={(value) => {
          if (isMode(value)) {
            setMode(value);
          }
        }}
        data={[
          { value: "off", label: "Off" },
          { value: "hosting", label: "Host a static website" },
          { value: "redirect", label: "Redirect all requests" },
        ]}
      />
      {draft.mode === "hosting" ? (
        <>
          <Group grow align="flex-start">
            <TextInput
              label="Index document"
              description="Served for requests to a folder."
              value={draft.index}
              onChange={(event) => change({ index: event.currentTarget.value })}
              spellCheck={false}
              autoComplete="off"
            />
            <TextInput
              label="Error document"
              description="Optional. Served when a request fails."
              placeholder="error.html"
              value={draft.error}
              onChange={(event) => change({ error: event.currentTarget.value })}
              spellCheck={false}
              autoComplete="off"
            />
          </Group>
          <Text size="sm" fw={500}>
            Routing rules
          </Text>
          {draft.rules.length === 0 ? (
            <Text size="sm" c="dimmed">
              No routing rules. A rule redirects requests that match its condition.
            </Text>
          ) : null}
          {draft.rules.map((rule, index) => (
            <RuleEditor
              key={rule.id}
              number={index + 1}
              rule={rule}
              onChange={(next) => change({ rules: draft.rules.map((item) => (item.id === rule.id ? next : item)) })}
              onRemove={() => change({ rules: draft.rules.filter((item) => item.id !== rule.id) })}
            />
          ))}
          <Group>
            <Button
              size="compact-sm"
              variant="default"
              disabled={draft.rules.length >= MAX_ROUTING_RULES}
              onClick={() => change({ rules: [...draft.rules, newRule()] })}
            >
              Add routing rule
            </Button>
          </Group>
        </>
      ) : null}
      {draft.mode === "redirect" ? (
        <Group grow align="flex-start">
          <TextInput
            label="Host name"
            description="Every request goes to this host."
            placeholder="www.example.com"
            value={draft.redirectHost}
            onChange={(event) => change({ redirectHost: event.currentTarget.value })}
            spellCheck={false}
            autoComplete="off"
          />
          <ProtocolSelect
            label="Protocol"
            value={draft.redirectProtocol}
            onChange={(redirectProtocol) => change({ redirectProtocol })}
          />
        </Group>
      ) : null}
      {attempted && problems.length > 0 ? (
        <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" title="Check the website settings">
          <List size="sm" spacing={2}>
            {problems.map((problem) => (
              <List.Item key={problem} style={{ overflowWrap: "anywhere" }}>
                {problem}
              </List.Item>
            ))}
          </List>
        </Alert>
      ) : null}
      <ActionError message={action.error} />
      <Group gap="xs">
        {draft.mode === "off" ? (
          <Button color="red" loading={action.busy} disabled={website === null} onClick={() => void disable()}>
            Disable website hosting
          </Button>
        ) : (
          <Button loading={action.busy} disabled={sameWebsite(wanted, website)} onClick={() => void save()}>
            Save website settings
          </Button>
        )}
      </Group>
    </Stack>
  );
}

export function WebsiteTab({ accountId, bucket, section, reload }: TabProps<WebsiteConfig | null>) {
  return (
    <SectionGate section={section} reload={reload}>
      {(website) => <WebsiteEditor accountId={accountId} bucket={bucket} website={website} reload={reload} />}
    </SectionGate>
  );
}
