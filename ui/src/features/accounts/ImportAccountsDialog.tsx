import { Alert, Button, Group, List, Radio, Stack, Text } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { DuplicatePolicy, ImportReport } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { tr } from "../../i18n";

function isPolicy(value: string): value is DuplicatePolicy {
  return value === "skip" || value === "rename";
}

function Report({ report }: { report: ImportReport }) {
  const needSecret = report.imported.filter((account) => account.needs_secret);
  return (
    <Stack gap="xs">
      <Text size="sm" role="status">
        {report.imported.length === 0
          ? tr("No account was imported.")
          : report.imported.length === 1
            ? tr("Imported {n} account.", { n: report.imported.length })
            : tr("Imported {n} accounts.", { n: report.imported.length })}
      </Text>
      {needSecret.length > 0 ? (
        <Alert color="blue" variant="light" title={tr("These accounts need their secret access key")}>
          <Text size="sm">
            {tr("The account file holds no secrets. Edit each account and enter its secret access key before you use it.")}
          </Text>
          <List size="sm" mt={4}>
            {needSecret.map((account) => (
              <List.Item key={account.id}>{account.label}</List.Item>
            ))}
          </List>
        </Alert>
      ) : null}
      {report.duplicates.length > 0 ? (
        <Alert color="yellow" variant="light" title={tr("Duplicates")}>
          <List size="sm">
            {report.duplicates.map((item) => (
              <List.Item key={`${item.label}:${item.reason}`}>
                {item.reason === "same_account"
                  ? tr("{label}: the same account as {existing}; left out.", { label: item.label, existing: item.existing })
                  : report.imported.some((account) => account.renamed_from === item.label)
                    ? tr("{label}: the label is taken by {existing}; imported under another label.", {
                        label: item.label,
                        existing: item.existing,
                      })
                    : tr("{label}: the label is taken by {existing}; left out.", { label: item.label, existing: item.existing })}
              </List.Item>
            ))}
          </List>
        </Alert>
      ) : null}
      {report.skipped.length > 0 ? (
        <Alert color="red" variant="light" title={tr("Left out")}>
          <List size="sm">
            {report.skipped.map((item) => (
              <List.Item key={item.label}>
                {item.label}: {item.reason}
              </List.Item>
            ))}
          </List>
        </Alert>
      ) : null}
    </Stack>
  );
}

/** Adds the accounts of an account file. Existing accounts are never changed; duplicates are reported. */
export function ImportAccountsDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const [path, setPath] = useState<string | null>(null);
  const [policy, setPolicy] = useState<DuplicatePolicy>("skip");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [report, setReport] = useState<ImportReport | null>(null);

  const choose = async () => {
    try {
      const picked = await model.platform.pickFiles(tr("Choose an account file"));
      if (picked?.[0] !== undefined) {
        setPath(picked[0]);
        setError(null);
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  const run = async () => {
    if (path === null) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const result = await model.api.import_accounts({ path, duplicates: policy });
      await model.session.refreshAccounts();
      setReport(result);
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  if (report !== null) {
    return (
      <OkCancelDialog opened title={tr("Import accounts")} okLabel={tr("Close")} onOk={onClose} onCancel={onClose} size="lg">
        <Report report={report} />
      </OkCancelDialog>
    );
  }
  return (
    <OkCancelDialog
      opened
      title={tr("Import accounts")}
      okLabel={tr("Import")}
      onOk={() => void run()}
      onCancel={onClose}
      okDisabled={path === null}
      busy={busy}
      error={error}
      size="lg"
    >
      <Text size="sm">
        {tr(
          "Choose a file written by Export accounts. It holds no secrets: imported accounts with access keys need their secret access key before you can use them. Existing accounts are never changed.",
        )}
      </Text>
      <Group gap="xs" wrap="nowrap">
        <Button variant="default" onClick={() => void choose()}>
          {tr("Choose file...")}
        </Button>
        <Text size="sm" c="dimmed" style={{ overflowWrap: "anywhere" }}>
          {path ?? tr("No file chosen")}
        </Text>
      </Group>
      <Radio.Group
        label={tr("An account whose label is already taken")}
        value={policy}
        onChange={(value) => {
          if (isPolicy(value)) {
            setPolicy(value);
          }
        }}
      >
        <Stack gap={6} mt={6}>
          <Radio value="skip" label={tr("Leave it out")} />
          <Radio value="rename" label={tr("Import it under another label")} description={tr("For example: Label (2).")} />
        </Stack>
      </Radio.Group>
    </OkCancelDialog>
  );
}
