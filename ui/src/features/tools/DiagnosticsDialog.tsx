import { Autocomplete, Button, Group, Select, Stack, Text } from "@mantine/core";
import { useState } from "react";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { validateBucketName } from "../../lib/validation";
import { DiagnosticsPanel } from "./DiagnosticsPanel";

/** Probes a stored account: DNS, TCP, proxy, TLS, credentials and, for a named bucket, its region. */
export function DiagnosticsDialog({ accountId: first, onClose }: { accountId?: string; onClose: () => void }) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const activeId = useSlice(model.session.store, (state) => state.activeAccountId);
  const bucketList = useSlice(model.browser.store, (state) => state.bucketList);
  const [chosen, setChosen] = useState<string | null>(first ?? null);
  const [bucket, setBucket] = useState("");
  // The active account unless another one was picked (or it no longer exists).
  const accountId = accounts.find((account) => account.id === (chosen ?? activeId))?.id ?? accounts[0]?.id ?? null;
  const trimmed = bucket.trim();
  const bucketError = trimmed.length > 0 ? validateBucketName(trimmed) : null;
  const suggestions = bucketList.accountId === accountId ? bucketList.buckets.map((item) => item.name) : [];
  return (
    <AppModal opened onClose={onClose} title={tr("Diagnostics")} size="lg" closeOnClickOutside={false}>
      {accountId === null ? (
        <Stack gap="sm">
          <Text size="sm">{tr("Add an account first: the diagnostics probe the connection of one account.")}</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              {tr("Close")}
            </Button>
          </Group>
        </Stack>
      ) : (
        <Stack gap="sm">
          <Text size="sm">
            {tr(
              "Checks the name lookup, the connection, the proxy, the TLS certificate, the credentials and, when you name a bucket, the region of that bucket, one step after the other with the time each takes.",
            )}
          </Text>
          <Select
            label={tr("Account")}
            data={accounts.map((account) => ({ value: account.id, label: account.label }))}
            value={accountId}
            allowDeselect={false}
            onChange={setChosen}
            comboboxProps={{ withinPortal: false }}
          />
          <Autocomplete
            label={tr("Bucket (optional)")}
            description={tr("Adds a check of the region of this bucket.")}
            value={bucket}
            onChange={setBucket}
            data={suggestions}
            error={bucketError}
            spellCheck={false}
            autoComplete="off"
            comboboxProps={{ withinPortal: false }}
          />
          <DiagnosticsPanel
            key={`${accountId}\u0000${trimmed}`}
            target={{ kind: "account", accountId }}
            bucket={trimmed.length > 0 ? trimmed : null}
            disabled={bucketError !== null}
          />
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              {tr("Close")}
            </Button>
          </Group>
        </Stack>
      )}
    </AppModal>
  );
}
