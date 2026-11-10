import { Alert, Button, Group, List, SegmentedControl, Stack, Text, Textarea } from "@mantine/core";
import { IconAlertCircle, IconCheck } from "@tabler/icons-react";
import { useEffect, useState, type ReactNode } from "react";
import type { RuleIssue } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { issueText } from "../../lib/policy";
import { ActionError } from "../bucketcfg/common";
import { useAction } from "../bucketcfg/useAction";

type Verdict = { type: "valid" } | { type: "issues"; issues: RuleIssue[] };

/**
 * The JSON view of one rule document: edit, validate (the backend lists every problem with its path), save and
 * delete. After a save the tab reads the setting again (`reload`), so the text shown is what the bucket has.
 */
export function DocEditor({
  label,
  noun,
  bucket,
  initial,
  example,
  validate,
  save,
  remove,
  reload,
  form,
  tools,
}: {
  /** Accessible name of the text area, such as "CORS rules (JSON)". */
  label: string;
  /** What the document is called in messages, such as "CORS rules". */
  noun: string;
  bucket: string;
  /** The document as read (pretty-printed JSON); `null` when nothing is configured. */
  initial: string | null;
  example: string;
  validate: (document: string) => Promise<RuleIssue[]>;
  save: (value: unknown) => Promise<void>;
  /** Deletes the configuration; `null` hides the button. */
  remove: (() => Promise<void>) | null;
  reload: () => Promise<void>;
  /** A second view of the same text (a form); the editor then offers "Form" next to "JSON". */
  form?: (text: string, setText: (text: string) => void) => ReactNode;
  /** Extra buttons above the editor (copy, paste, import, export); they change the text through `setText`. */
  tools?: (text: string, setText: (text: string) => void) => ReactNode;
}) {
  const model = useModel();
  const [text, setText] = useState(initial ?? "");
  const [verdict, setVerdict] = useState<Verdict | null>(null);
  const action = useAction();
  const unchanged = text === (initial ?? "");
  const [view, setView] = useState<"json" | "form">("json");
  const edit = (next: string) => {
    setText(next);
    setVerdict(null);
  };

  // In the form view the text is checked a moment after the last change, so the problems show up while editing.
  useEffect(() => {
    if (form === undefined || view !== "form" || text.trim().length === 0) {
      return;
    }
    let live = true;
    const timer = setTimeout(() => {
      validate(text)
        .then((issues) => live && setVerdict(issues.length === 0 ? { type: "valid" } : { type: "issues", issues }))
        .catch(() => undefined);
    }, 400);
    return () => {
      live = false;
      clearTimeout(timer);
    };
    // `validate` is a new function on every render; the text and the view decide when to check again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [form, view, text]);

  const check = async (): Promise<boolean> => {
    const issues = await validate(text);
    setVerdict(issues.length === 0 ? { type: "valid" } : { type: "issues", issues });
    return issues.length === 0;
  };
  const doSave = () =>
    action.run(async () => {
      if (!(await check())) {
        return;
      }
      await save(JSON.parse(text) as unknown);
      model.status.info(tr("The {noun} of {bucket} were saved.", { noun, bucket }));
      await reload();
    });
  const doRemove = async () => {
    if (remove === null) {
      return;
    }
    const confirmed = await model.dialogs.confirm({
      title: tr("Delete {noun}", { noun }),
      message: tr("The {noun} of {bucket} are deleted.\n\nThis cannot be undone.", { noun, bucket }),
      confirmLabel: tr("Delete"),
      danger: true,
      typedConfirmation: "delete",
    });
    if (!confirmed) {
      return;
    }
    await action.run(async () => {
      await remove();
      model.status.info(tr("The {noun} of {bucket} were deleted.", { noun, bucket }));
      await reload();
    });
  };

  return (
    <Stack gap="sm">
      <Text size="sm" c="dimmed">
        {initial === null
          ? tr("This bucket has no {noun}. Write them below and save.", { noun })
          : tr("The {noun} of this bucket. Edit, validate and save.", { noun })}
      </Text>
      {tools?.(text, edit)}
      {form !== undefined ? (
        <SegmentedControl
          aria-label={tr("View")}
          value={view}
          onChange={(value) => setView(value === "form" ? "form" : "json")}
          data={[
            { value: "json", label: "JSON" },
            { value: "form", label: tr("Form") },
          ]}
        />
      ) : null}
      {form !== undefined && view === "form" ? (
        form(text, edit)
      ) : (
        <Textarea
          label={label}
          value={text}
          onChange={(event) => edit(event.currentTarget.value)}
          placeholder={example}
          rows={14}
          resize="vertical"
          spellCheck={false}
          autoComplete="off"
          styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)", fontSize: 12 } }}
        />
      )}
      {verdict?.type === "valid" ? (
        <Alert color="teal" icon={<IconCheck size={16} />} py="xs" aria-live="polite">
          {tr("The document is valid.")}
        </Alert>
      ) : null}
      {verdict?.type === "issues" ? (
        <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" title={verdict.issues.length === 1 ? tr("1 problem") : tr("{n} problems", { n: verdict.issues.length })}>
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
        <Button variant="default" disabled={action.busy || text.trim().length === 0} onClick={() => void action.run(async () => void (await check()))}>
          {tr("Validate")}
        </Button>
        <Button loading={action.busy} disabled={text.trim().length === 0 || unchanged} onClick={() => void doSave()}>
          {tr("Save")}
        </Button>
        {remove !== null && initial !== null ? (
          <Button color="red" variant="light" ml="auto" disabled={action.busy} onClick={() => void doRemove()}>
            {tr("Delete")}
          </Button>
        ) : null}
      </Group>
    </Stack>
  );
}
