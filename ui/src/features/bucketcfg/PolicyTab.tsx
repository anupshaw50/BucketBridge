import { Alert, Button, Group, List, Stack, Text, Textarea } from "@mantine/core";
import { IconAlertCircle, IconCheck } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { PolicyIssue } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { issueText } from "../../lib/policy";
import { ActionError, SectionGate, type TabProps } from "./common";
import { useAction } from "./useAction";

const EXAMPLE = `{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "PublicRead",
      "Effect": "Allow",
      "Principal": "*",
      "Action": "s3:GetObject",
      "Resource": "arn:aws:s3:::example-bucket/*"
    }
  ]
}`;

/** What the last Validate or Save found. */
type Verdict = { type: "valid" } | { type: "issues"; issues: PolicyIssue[] };

function PolicyEditor({ accountId, bucket, policy, reload }: { accountId: string; bucket: string; policy: string | null; reload: () => Promise<void> }) {
  const model = useModel();
  const [text, setText] = useState(policy ?? "");
  const [verdict, setVerdict] = useState<Verdict | null>(null);
  const action = useAction();
  const unchanged = text === (policy ?? "");

  const edit = (next: string) => {
    setText(next);
    setVerdict(null);
  };
  const check = async (): Promise<boolean> => {
    const issues = await model.api.validate_bucket_policy({ policy: text });
    setVerdict(issues.length === 0 ? { type: "valid" } : { type: "issues", issues });
    return issues.length === 0;
  };
  const format = () =>
    action.run(async () => {
      try {
        edit(await model.api.format_bucket_policy({ policy: text }));
      } catch (raw) {
        // Only the JSON syntax can be wrong here: show it like a validation problem.
        const error = toBbError(raw);
        if (error.code !== "usage") {
          throw error;
        }
        setVerdict({ type: "issues", issues: [{ path: "", message: error.message }] });
      }
    });
  const validate = () =>
    action.run(async () => {
      await check();
    });
  const save = () =>
    action.run(async () => {
      if (!(await check())) {
        return;
      }
      await model.api.put_bucket_policy({ account_id: accountId, bucket, policy: text });
      model.status.info(tr("The policy of {bucket} was saved.", { bucket }));
      await reload();
    });
  const remove = async () => {
    const confirmed = await model.dialogs.confirm({
      title: tr("Delete bucket policy"),
      message: tr("The policy of {bucket} is deleted. Access to the bucket then depends only on its other settings, such as IAM policies and ACLs.\n\nThis cannot be undone.", { bucket }),
      confirmLabel: tr("Delete policy"),
      danger: true,
    });
    if (!confirmed) {
      return;
    }
    await action.run(async () => {
      await model.api.delete_bucket_policy({ account_id: accountId, bucket });
      model.status.info(tr("The policy of {bucket} was deleted.", { bucket }));
      await reload();
    });
  };

  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {policy === null
          ? tr("This bucket has no policy. Write one below and save it.")
          : tr("The policy attached to this bucket. Edit it, then validate and save.")}
      </Text>
      <Textarea
        label={tr("Bucket policy (JSON)")}
        value={text}
        onChange={(event) => edit(event.currentTarget.value)}
        placeholder={EXAMPLE}
        rows={14}
        resize="vertical"
        spellCheck={false}
        autoComplete="off"
        styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)", fontSize: 12 } }}
      />
      {verdict?.type === "valid" ? (
        <Alert color="teal" icon={<IconCheck size={16} />} py="xs" aria-live="polite">
          {tr("The policy is valid.")}
        </Alert>
      ) : null}
      {verdict?.type === "issues" ? (
        <Alert
          color="red"
          icon={<IconAlertCircle size={16} />}
          role="alert"
          title={verdict.issues.length === 1 ? tr("1 problem") : tr("{n} problems", { n: verdict.issues.length })}
        >
          <List size="sm" spacing={2}>
            {verdict.issues.map((issue) => (
              <List.Item key={`${issue.path}|${issue.message}`} style={{ overflowWrap: "anywhere" }}>
                {issueText(issue)}
              </List.Item>
            ))}
          </List>
        </Alert>
      ) : null}
      <ActionError message={action.error} />
      <Group gap="xs">
        <Button variant="default" disabled={action.busy || text.trim().length === 0} onClick={() => void validate()}>
          {tr("Validate")}
        </Button>
        <Button variant="default" disabled={action.busy || text.trim().length === 0} onClick={() => void format()}>
          {tr("Format")}
        </Button>
        <Button loading={action.busy} disabled={text.trim().length === 0 || unchanged} onClick={() => void save()}>
          {tr("Save policy")}
        </Button>
        {policy !== null ? (
          <Button color="red" variant="light" ml="auto" disabled={action.busy} onClick={() => void remove()}>
            {tr("Delete policy")}
          </Button>
        ) : null}
      </Group>
    </Stack>
  );
}

export function PolicyTab({ accountId, bucket, section, reload }: TabProps<string | null>) {
  return (
    <SectionGate section={section} reload={reload}>
      {(policy) => <PolicyEditor accountId={accountId} bucket={bucket} policy={policy} reload={reload} />}
    </SectionGate>
  );
}
