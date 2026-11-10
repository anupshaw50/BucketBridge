import { Checkbox, PasswordInput, SegmentedControl, Stack, Switch, TagsInput, Text, TextInput } from "@mantine/core";
import type { Account } from "../../api/types";
import { RegionInput } from "../../components/RegionInput";
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
      <RegionInput
        extraRegions={draft.compat_regions}
        label={tr("Region")}
        description={tr("Pick a region or type its name; China and GovCloud regions use their own partition.")}
        value={draft.region}
        onChange={(region) => onChange({ region })}
        error={errors.region}
        required
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
      <Switch
        label={tr("Use Transfer Acceleration")}
        description={tr("Transfers use the accelerated endpoint of buckets that have acceleration enabled. AWS only.")}
        checked={draft.accelerate}
        onChange={(event) => onChange({ accelerate: event.currentTarget.checked })}
        error={errors.accelerate}
      />
      <Switch
        label={tr("Use dual-stack endpoints")}
        description={tr("Reach AWS over IPv4 and IPv6. AWS only.")}
        checked={draft.dual_stack}
        onChange={(event) => onChange({ dual_stack: event.currentTarget.checked })}
      />
      <ProxyFields draft={draft} errors={errors} onChange={onChange} />
      <Switch
        label={tr("Cannot list buckets")}
        description={tr("For accounts with limited permissions, such as write-only access: the bucket list is not requested and shows the buckets below.")}
        checked={draft.cannot_list_buckets}
        onChange={(event) => onChange({ cannot_list_buckets: event.currentTarget.checked })}
      />
      {draft.cannot_list_buckets || draft.known_buckets.length > 0 ? (
        <TagsInput
          label={tr("Known buckets and folders")}
          description={tr("Type a bucket name, or bucket/folder/ to open it in that folder.")}
          value={draft.known_buckets}
          onChange={(known_buckets) => onChange({ known_buckets })}
          error={errors.known_buckets}
        />
      ) : null}
      {draft.endpoint.trim().length > 0 ? <ProviderFields draft={draft} errors={errors} onChange={onChange} /> : null}
    </Stack>
  );
}

/** How the account uses the proxy: the app's, none, or its own. */
function ProxyFields({ draft, errors, onChange }: FieldsProps) {
  return (
    <Stack gap="xs">
      <Text size="sm" fw={500}>
        {tr("Proxy")}
      </Text>
      <SegmentedControl
        aria-label={tr("Proxy")}
        value={draft.proxy_mode}
        onChange={(value) => onChange({ proxy_mode: value })}
        data={[
          { value: "global", label: tr("App proxy") },
          { value: "none", label: tr("No proxy") },
          { value: "custom", label: tr("Custom") },
        ]}
      />
      <Text size="xs" c="dimmed">
        {draft.proxy_mode === "global"
          ? tr("Uses the proxy of the app settings, if there is one.")
          : draft.proxy_mode === "none"
            ? tr("Connects directly, ignoring the app proxy and the proxy environment variables.")
            : tr("Uses this proxy for this account only. A proxy that needs a password is set in the app settings.")}
      </Text>
      {draft.proxy_mode === "custom" ? (
        <Stack gap="xs">
          <SegmentedControl
            aria-label={tr("Proxy type")}
            value={draft.proxy_kind}
            onChange={(value) => onChange({ proxy_kind: value })}
            data={[
              { value: "http", label: "HTTP" },
              { value: "socks5", label: "SOCKS5" },
            ]}
          />
          <TextInput
            label={tr("Proxy host")}
            value={draft.proxy_host}
            onChange={(event) => onChange({ proxy_host: event.currentTarget.value })}
            error={errors.proxy_host}
            spellCheck={false}
          />
          <TextInput
            label={tr("Proxy port")}
            value={draft.proxy_port}
            onChange={(event) => onChange({ proxy_port: event.currentTarget.value })}
            error={errors.proxy_port}
            inputMode="numeric"
          />
          <TagsInput
            label={tr("Hosts that bypass the proxy")}
            description={tr("Host names, *.suffix or address ranges.")}
            value={draft.proxy_bypass}
            onChange={(proxy_bypass) => onChange({ proxy_bypass })}
          />
        </Stack>
      ) : null}
    </Stack>
  );
}

/** Settings of S3-compatible providers (shown with a custom endpoint). */
function ProviderFields({ draft, errors, onChange }: FieldsProps) {
  return (
    <Stack gap="sm">
      <Text fw={600} size="sm">
        {tr("S3-compatible provider settings")}
      </Text>
      <TagsInput
        label={tr("Storage classes")}
        description={tr("The classes this provider offers, for example HOT or COLD. Leave empty for the AWS list.")}
        value={draft.compat_storage_classes}
        onChange={(compat_storage_classes) => onChange({ compat_storage_classes })}
        error={errors.compat_storage_classes}
      />
      <TagsInput
        label={tr("Regions")}
        description={tr("The regions this provider has. Leave empty for the AWS list.")}
        value={draft.compat_regions}
        onChange={(compat_regions) => onChange({ compat_regions })}
        error={errors.compat_regions}
      />
      <Switch
        label={tr("List buckets in every region")}
        description={tr("Ask once per region above and merge the answers, for providers that list one region at a time.")}
        checked={draft.list_all_regions}
        onChange={(event) => onChange({ list_all_regions: event.currentTarget.checked })}
      />
      <Switch
        label={tr("Delete objects one at a time")}
        description={tr("For providers without multi-object delete.")}
        checked={draft.single_deletes}
        onChange={(event) => onChange({ single_deletes: event.currentTarget.checked })}
      />
      <Switch
        label={tr("Do not copy in parts")}
        description={tr("Copy with a single request, which works up to 5 GiB. Larger objects must be downloaded and uploaded.")}
        checked={draft.no_multipart_copy}
        onChange={(event) => onChange({ no_multipart_copy: event.currentTarget.checked })}
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
