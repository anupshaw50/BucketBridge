import { Checkbox, PasswordInput, SegmentedControl, Stack, Switch, Text, TextInput } from "@mantine/core";
import type { Account } from "../../api/types";
import { tr } from "../../i18n";
import type { AccountDraft, DraftErrors, SessionTokenMode } from "../../lib/accounts";

export interface FieldsProps {
  draft: AccountDraft;
  errors: DraftErrors;
  onChange: (patch: Partial<AccountDraft>) => void;
}

/** Label, endpoint, region and addressing options. */
export function ConnectionFields({ draft, errors, onChange, autoFocus = true }: FieldsProps & { autoFocus?: boolean }) {
  return (
    <Stack gap="sm">
      <TextInput
        label={tr("Label")}
        description={tr("A unique name for this account.")}
        value={draft.label}
        onChange={(event) => onChange({ label: event.currentTarget.value })}
        error={errors.label}
        required
        data-autofocus={autoFocus ? true : undefined}
      />
      <TextInput
        label={tr("Endpoint")}
        description={tr("Leave empty for AWS. For other S3-compatible services enter the endpoint URL.")}
        placeholder="https://storage.example.com"
        value={draft.endpoint}
        onChange={(event) => onChange({ endpoint: event.currentTarget.value })}
        error={errors.endpoint}
        spellCheck={false}
      />
      <TextInput
        label={tr("Region")}
        value={draft.region}
        onChange={(event) => onChange({ region: event.currentTarget.value })}
        error={errors.region}
        required
        spellCheck={false}
      />
      <Switch
        label={tr("Use path-style addressing")}
        description={tr("Needed by some S3-compatible services that do not support bucket host names.")}
        checked={draft.path_style}
        onChange={(event) => onChange({ path_style: event.currentTarget.checked })}
      />
      <Switch
        label={tr("Requester pays")}
        description={tr("Accept charges when reading from requester-pays buckets.")}
        checked={draft.request_payer}
        onChange={(event) => onChange({ request_payer: event.currentTarget.checked })}
      />
    </Stack>
  );
}

export interface CredentialFieldsProps extends FieldsProps {
  /** The account being edited, or null when adding. */
  original: Account | null;
  /** Puts the cursor in the MFA device field when the dialog opens. */
  focusMfa?: boolean;
}

/** Access key, secret (show/hide), optional session token and MFA device. */
export function CredentialFields({ draft, errors, onChange, original, focusMfa = false }: CredentialFieldsProps) {
  const editing = original !== null;
  const hasToken = original !== null && original.session_token_ref !== null;
  return (
    <Stack gap="sm">
      <TextInput
        label={tr("Access key ID")}
        value={draft.access_key_id}
        onChange={(event) => onChange({ access_key_id: event.currentTarget.value })}
        error={errors.access_key_id}
        required
        spellCheck={false}
        autoComplete="off"
        data-autofocus={editing ? undefined : true}
      />
      <PasswordInput
        label={tr("Secret access key")}
        description={editing ? tr("Leave empty to keep the stored secret. Enter a new one to rotate it.") : undefined}
        placeholder={editing ? tr("Unchanged") : undefined}
        value={draft.secret_access_key}
        onChange={(event) => onChange({ secret_access_key: event.currentTarget.value })}
        error={errors.secret_access_key}
        required={!editing}
        autoComplete="new-password"
        visibilityToggleButtonProps={{ "aria-label": tr("Show or hide the secret access key") }}
      />
      {editing && hasToken ? (
        <Stack gap={4}>
          <Text size="sm" fw={500}>
            {tr("Session token")}
          </Text>
          <SegmentedControl<SessionTokenMode>
            size="xs"
            value={draft.session_token_mode}
            onChange={(value) => onChange({ session_token_mode: value })}
            data={[
              { value: "keep", label: tr("Keep") },
              { value: "replace", label: tr("Replace") },
              { value: "clear", label: tr("Remove") },
            ]}
            aria-label={tr("Session token")}
          />
        </Stack>
      ) : null}
      {editing && !hasToken ? (
        <Checkbox
          label={tr("Add a session token")}
          checked={draft.session_token_mode === "replace"}
          onChange={(event) => onChange({ session_token_mode: event.currentTarget.checked ? "replace" : "keep" })}
        />
      ) : null}
      {!editing || draft.session_token_mode === "replace" ? (
        <PasswordInput
          label={editing ? tr("New session token") : tr("Session token (optional)")}
          description={tr("Only for temporary credentials.")}
          value={draft.session_token}
          onChange={(event) => onChange({ session_token: event.currentTarget.value })}
          error={errors.session_token}
          autoComplete="off"
          visibilityToggleButtonProps={{ "aria-label": tr("Show or hide the session token") }}
        />
      ) : null}
      <TextInput
        label={tr("MFA device (optional)")}
        description={tr("ARN or serial number of the MFA device. You will be asked for a code when needed.")}
        value={draft.mfa_serial}
        onChange={(event) => onChange({ mfa_serial: event.currentTarget.value })}
        error={errors.mfa_serial}
        spellCheck={false}
        autoComplete="off"
        data-autofocus={focusMfa ? true : undefined}
      />
    </Stack>
  );
}
