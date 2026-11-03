import { Alert, Badge, Checkbox, List, Loader, Stack, Table, Text } from "@mantine/core";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { AwsProfileEntry, AwsProfileKind, ImportOutcome } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";

const KIND_LABELS: Record<AwsProfileKind, string> = {
  static_keys: "Access keys",
  assume_role: "Role",
  sso: "AWS SSO",
  credential_process: "Credential process",
  web_identity: "Web identity",
  incomplete: "Incomplete",
};

/** Lists the profiles of the AWS config files and links the chosen ones as accounts (no secret is copied). */
export function ImportAwsDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const [profiles, setProfiles] = useState<AwsProfileEntry[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [chosen, setChosen] = useState<ReadonlySet<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<ImportOutcome | null>(null);

  useEffect(() => {
    let current = true;
    model.api
      .list_aws_profiles()
      .then((listed) => {
        if (current) {
          setProfiles(listed);
          setChosen(new Set(listed.filter(isFree).map((item) => item.name)));
        }
      })
      .catch((raw: unknown) => {
        if (current) {
          setLoadError(toBbError(raw).message);
          setProfiles([]);
        }
      });
    return () => {
      current = false;
    };
  }, [model]);

  const toggle = (name: string, on: boolean) => {
    setChosen((existing) => {
      const next = new Set(existing);
      if (on) {
        next.add(name);
      } else {
        next.delete(name);
      }
      return next;
    });
  };

  const run = async () => {
    setBusy(true);
    setError(null);
    try {
      const result = await model.api.import_aws_profiles({ names: [...chosen] });
      await model.session.refreshAccounts();
      setOutcome(result);
      if (result.imported.length > 0) {
        model.status.info(`Imported ${result.imported.length} AWS profile${result.imported.length === 1 ? "" : "s"}.`);
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  if (outcome !== null) {
    return (
      <OkCancelDialog opened title="Import from AWS config" okLabel="Close" onOk={onClose} onCancel={onClose} size="lg">
        <Stack gap="xs">
          <Text size="sm">
            {outcome.imported.length === 0
              ? "No profile was imported."
              : `Imported ${outcome.imported.length} profile${outcome.imported.length === 1 ? "" : "s"}: ${outcome.imported
                  .map((account) => account.label)
                  .join(", ")}.`}
          </Text>
          {outcome.skipped.length > 0 ? (
            <Alert color="yellow" variant="light" title="Left out">
              <List size="sm">
                {outcome.skipped.map((item) => (
                  <List.Item key={item.profile}>
                    {item.profile}: {item.reason}
                  </List.Item>
                ))}
              </List>
            </Alert>
          ) : null}
        </Stack>
      </OkCancelDialog>
    );
  }

  return (
    <OkCancelDialog
      opened
      title="Import from AWS config"
      okLabel="Import"
      onOk={() => void run()}
      onCancel={onClose}
      okDisabled={chosen.size === 0 || profiles === null}
      busy={busy}
      error={error ?? loadError}
      size="lg"
    >
      <Text size="sm">
        Choose profiles from your AWS config and credentials files. The accounts stay linked to the files: no key is copied into
        BucketBridge, and changes you make in the files apply the next time credentials are needed.
      </Text>
      {profiles === null ? (
        <Loader size="sm" aria-label="Reading the AWS files" />
      ) : profiles.length === 0 ? (
        <Text size="sm" c="dimmed" py="md">
          No profiles were found in the AWS config files.
        </Text>
      ) : (
        <Table.ScrollContainer minWidth={520} maxHeight={320}>
          <Table verticalSpacing={6} fz="sm" aria-label="AWS profiles">
            <Table.Thead>
              <Table.Tr>
                <Table.Th w={40}>
                  <span className="bb-visually-hidden">Import</span>
                </Table.Th>
                <Table.Th>Profile</Table.Th>
                <Table.Th>Type</Table.Th>
                <Table.Th>Region</Table.Th>
                <Table.Th>Status</Table.Th>
              </Table.Tr>
            </Table.Thead>
            <Table.Tbody>
              {profiles.map((item) => (
                <Table.Tr key={item.name}>
                  <Table.Td>
                    <Checkbox
                      aria-label={`Import ${item.name}`}
                      checked={chosen.has(item.name)}
                      disabled={!isFree(item)}
                      onChange={(event) => toggle(item.name, event.currentTarget.checked)}
                    />
                  </Table.Td>
                  <Table.Td fw={500}>{item.name}</Table.Td>
                  <Table.Td>{KIND_LABELS[item.kind]}</Table.Td>
                  <Table.Td>{item.region ?? "-"}</Table.Td>
                  <Table.Td>
                    {item.account_id !== null ? (
                      <Badge size="xs" variant="light">
                        Already imported
                      </Badge>
                    ) : item.importable ? null : (
                      <Text size="xs" c="dimmed">
                        {item.note}
                      </Text>
                    )}
                  </Table.Td>
                </Table.Tr>
              ))}
            </Table.Tbody>
          </Table>
        </Table.ScrollContainer>
      )}
    </OkCancelDialog>
  );
}

function isFree(item: AwsProfileEntry): boolean {
  return item.importable && item.account_id === null;
}
