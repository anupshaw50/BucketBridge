import { Button, Group, Select, Stack, Text } from "@mantine/core";
import { IconWorldUpload } from "@tabler/icons-react";
import { useState } from "react";
import { errorText } from "../../api/errors";
import type { ShortenerProvider } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { tr } from "../../i18n";
import { CopyField } from "../../components/CopyField";
import { shortenerName } from "./shortener";

/**
 * Optional URL shortening of a share link. Off unless the user chose a service in the settings, and
 * nothing is sent until the button is pressed: the text says the whole link leaves this machine.
 */
export function ShortenControl({ url }: { url: string }) {
  const model = useModel();
  const configured = useSlice(model.settings.store, (state) => state.settings.shortener.provider);
  const [choice, setChoice] = useState<ShortenerProvider | null>(null);
  const [short, setShort] = useState<{ source: string; link: string; provider: ShortenerProvider } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const provider = choice ?? configured;
  if (configured === "none") {
    return (
      <Group gap="xs">
        <Text size="xs" c="dimmed">
          {tr("URL shortening is off. Share links are never sent to a third party unless you choose a service.")}
        </Text>
        <Button size="compact-xs" variant="subtle" onClick={() => model.dialogs.open({ kind: "settings", page: "sharing" })}>
          {tr("Choose a service...")}
        </Button>
      </Group>
    );
  }
  const name = shortenerName(provider);
  const shorten = async () => {
    setBusy(true);
    setError(null);
    try {
      const link = await model.api.shorten_url({ url, provider });
      setShort({ source: url, link, provider });
    } catch (failure) {
      setError(errorText(failure));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Stack gap={6}>
      <Group gap="xs" align="flex-end">
        <Select
          label={tr("Service")}
          w={130}
          size="xs"
          allowDeselect={false}
          value={provider}
          data={[
            { value: "bitly", label: "Bitly" },
            { value: "tinyurl", label: "TinyURL" },
          ]}
          onChange={(value) => setChoice(value === "bitly" || value === "tinyurl" ? value : null)}
        />
        <Button
          size="xs"
          variant="default"
          leftSection={<IconWorldUpload size={14} aria-hidden />}
          loading={busy}
          onClick={() => void shorten()}
        >
          {tr("Shorten")}
        </Button>
        <Text size="xs" c="dimmed">
          {tr("Shorten sends the whole link, including its signature, to {name}, outside this machine.", { name })}
        </Text>
      </Group>
      {error !== null ? (
        <Text size="xs" c="red" role="alert">
          {error}
        </Text>
      ) : null}
      {short !== null && short.source === url && short.provider === provider ? (
        <>
          <CopyField label={tr("Short link")} value={short.link} sensitive />
          <Text size="xs" c="dimmed">
            {tr("The short link keeps redirecting after the presigned link expires, but the target stops working.")}
          </Text>
        </>
      ) : null}
    </Stack>
  );
}
