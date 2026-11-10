import { Button, Group } from "@mantine/core";
import { IconClipboard, IconCopy, IconFileExport, IconFileImport } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { copiedRules, copyRules, mergeRules, parseRules, rulesText } from "./lifecycleModel";
import { ActionError } from "../bucketcfg/common";

/**
 * Copy and paste of lifecycle rules between buckets, and import and export of the document as a `.json` file. They
 * only change the text of the editor; nothing reaches the bucket until the editor saves.
 */
export function LifecycleTools({ bucket, text, setText }: { bucket: string; text: string; setText: (text: string) => void }) {
  const model = useModel();
  const [error, setError] = useState<string | null>(null);
  const [canPaste, setCanPaste] = useState(copiedRules() !== null);

  const guard = async (work: () => Promise<void>) => {
    setError(null);
    try {
      await work();
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  const copy = () =>
    guard(async () => {
      const parsed = parseRules(text);
      if (!parsed.ok) {
        throw new Error(tr("The JSON cannot be read: {message}", { message: parsed.message }));
      }
      if (parsed.config.rules.length === 0) {
        throw new Error(tr("There are no rules to copy."));
      }
      copyRules(parsed.config.rules);
      setCanPaste(true);
      await model.platform.copyText(rulesText(parsed.config));
      model.status.info(tr("Copied {n} rules of {bucket}. Open another bucket and paste them.", { n: parsed.config.rules.length, bucket }));
    });

  const paste = () =>
    guard(() => {
      const pasted = copiedRules();
      if (pasted === null) {
        throw new Error(tr("Copy rules first."));
      }
      const parsed = parseRules(text);
      if (!parsed.ok) {
        throw new Error(tr("The JSON cannot be read: {message}", { message: parsed.message }));
      }
      setText(rulesText({ ...parsed.config, rules: mergeRules(parsed.config.rules, pasted) }));
      model.status.info(tr("Pasted {n} rules. Validate and save to apply them to {bucket}.", { n: pasted.length, bucket }));
      return Promise.resolve();
    });

  const importFile = () =>
    guard(async () => {
      const picked = await model.platform.pickFiles(tr("Import lifecycle rules"));
      const path = picked?.[0];
      if (path === undefined) {
        return;
      }
      setText(await model.api.read_rules_file({ path }));
    });

  const exportFile = () =>
    guard(async () => {
      const parsed = parseRules(text);
      if (!parsed.ok) {
        throw new Error(tr("The JSON cannot be read: {message}", { message: parsed.message }));
      }
      const path = await model.platform.pickSavePath({
        title: tr("Export lifecycle rules"),
        defaultPath: `${bucket}-lifecycle.json`,
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (path === null) {
        return;
      }
      await model.api.write_rules_file({ path, document: rulesText(parsed.config) });
      model.status.info(tr("Exported the lifecycle rules to {path}.", { path }));
    });

  return (
    <>
      <Group gap="xs">
        <Button size="compact-sm" variant="default" leftSection={<IconCopy size={14} />} onClick={() => void copy()}>
          {tr("Copy rules")}
        </Button>
        <Button size="compact-sm" variant="default" leftSection={<IconClipboard size={14} />} disabled={!canPaste} onClick={() => void paste()}>
          {tr("Paste rules")}
        </Button>
        <Button size="compact-sm" variant="default" leftSection={<IconFileImport size={14} />} onClick={() => void importFile()}>
          {tr("Import from file...")}
        </Button>
        <Button size="compact-sm" variant="default" leftSection={<IconFileExport size={14} />} onClick={() => void exportFile()}>
          {tr("Export to file...")}
        </Button>
      </Group>
      <ActionError message={error} />
    </>
  );
}
