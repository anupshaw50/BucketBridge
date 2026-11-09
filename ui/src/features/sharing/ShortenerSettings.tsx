import { Alert, Button, Group, PasswordInput, Select, Stack, Text } from "@mantine/core";
import { IconInfoCircle } from "@tabler/icons-react";
import { useState } from "react";
import { errorText } from "../../api/errors";
import type { ShortenerProvider } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";

const PROVIDERS: { value: ShortenerProvider; label: string }[] = [
  { value: "none", label: tr("None (default)") },
  { value: "bitly", label: "Bitly" },
  { value: "tinyurl", label: "TinyURL" },
];

/** The Sharing page of the settings: which shortener a share link can be sent to, and its API token. */
export function ShortenerSettings({
  provider,
  tokenSet,
  onProvider,
  onTokenSaved,
}: {
  provider: ShortenerProvider;
  tokenSet: boolean;
  onProvider: (provider: ShortenerProvider) => void;
  onTokenSaved: (ref: string | null) => void;
}) {
  const model = useModel();
  const [editing, setEditing] = useState(false);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const store = async (value: string | null) => {
    setBusy(true);
    setError(null);
    try {
      await model.api.set_shortener_token({ token: value });
      onTokenSaved(await model.settings.refreshShortenerTokenRef());
      model.status.info(value === null ? tr("Shortener token removed.") : tr("Shortener token saved."));
      setEditing(false);
      setToken("");
    } catch (failure) {
      setError(errorText(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Stack gap="sm">
      <Alert color="yellow" icon={<IconInfoCircle size={16} />} py="xs">
        {tr("A URL shortener is an outside service. When you press Shorten on a share link, the whole link, including the signature of a presigned link, is sent to it. Nothing is sent otherwise, and nothing is sent while this is set to None.")}
      </Alert>
      <Select
        label={tr("URL shortener")}
        value={provider}
        allowDeselect={false}
        data={PROVIDERS}
        w={260}
        onChange={(value) => {
          if (value === "none" || value === "bitly" || value === "tinyurl") {
            onProvider(value);
          }
        }}
      />
      {provider !== "none" ? (
        <Stack gap={6}>
          <Text size="sm" fw={500}>
            {tr("API token")}
          </Text>
          <Text size="sm" c="dimmed">
            {tokenSet ? tr("A token is stored in the credential store.") : tr("No token is stored.")}{" "}
            {provider === "bitly" ? tr("Bitly needs a token.") : tr("TinyURL works without a token.")}{" "}
            {tr("Tokens are never shown again; changes here apply immediately.")}
          </Text>
          {editing ? (
            <Group align="flex-end" gap="xs">
              <PasswordInput
                label={tr("New token")}
                value={token}
                onChange={(event) => setToken(event.currentTarget.value)}
                w={260}
                autoComplete="off"
                data-autofocus
              />
              <Button size="xs" loading={busy} disabled={token.length === 0} onClick={() => void store(token)}>
                {tr("Save token")}
              </Button>
              <Button size="xs" variant="default" onClick={() => setEditing(false)}>
                {tr("Cancel")}
              </Button>
            </Group>
          ) : (
            <Group gap="xs">
              <Button size="xs" variant="default" onClick={() => setEditing(true)}>
                {tokenSet ? tr("Change token...") : tr("Set token...")}
              </Button>
              {tokenSet ? (
                <Button size="xs" variant="subtle" color="red" loading={busy} onClick={() => void store(null)}>
                  {tr("Remove token")}
                </Button>
              ) : null}
            </Group>
          )}
          {error !== null ? (
            <Text size="xs" c="red" role="alert">
              {error}
            </Text>
          ) : null}
        </Stack>
      ) : null}
    </Stack>
  );
}
