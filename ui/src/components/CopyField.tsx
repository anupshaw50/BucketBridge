import { ActionIcon, Group, Textarea, TextInput, Tooltip } from "@mantine/core";
import { IconCheck, IconCopy } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { useModel } from "../app/context";

export interface CopyFieldProps {
  label: string;
  value: string;
  multiline?: boolean;
  /** Copies as a sensitive value (kept out of clipboard history where supported). */
  sensitive?: boolean;
}

/** Read-only value with a copy-to-clipboard button. */
export function CopyField({ label, value, multiline = false, sensitive = false }: CopyFieldProps) {
  const model = useModel();
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) {
      return undefined;
    }
    const timer = setTimeout(() => setCopied(false), 1500);
    return () => clearTimeout(timer);
  }, [copied]);
  const copy = async () => {
    try {
      await model.platform.copyText(value, { sensitive });
      setCopied(true);
    } catch (error) {
      model.status.error(error, { notify: "Could not copy to the clipboard" });
    }
  };
  const button = (
    <Tooltip label={copied ? "Copied" : "Copy to clipboard"} withinPortal={false}>
      <ActionIcon
        variant="subtle"
        aria-label={`Copy ${label.toLowerCase()}`}
        onClick={copy}
        disabled={value.length === 0}
        color={copied ? "teal" : undefined}
      >
        {copied ? <IconCheck size={16} /> : <IconCopy size={16} />}
      </ActionIcon>
    </Tooltip>
  );
  if (multiline) {
    return (
      <Group align="flex-end" gap="xs" wrap="nowrap">
        <Textarea
          label={label}
          value={value}
          readOnly
          rows={4}
          resize="vertical"
          style={{ flex: 1 }}
          styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)", fontSize: 12 } }}
        />
        {button}
      </Group>
    );
  }
  return (
    <TextInput
      label={label}
      value={value}
      readOnly
      rightSection={button}
      styles={{ input: { fontFamily: "var(--mantine-font-family-monospace)" } }}
      onFocus={(event) => event.currentTarget.select()}
    />
  );
}
