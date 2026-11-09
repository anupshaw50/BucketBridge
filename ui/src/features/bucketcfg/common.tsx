import { Alert, Button, Text } from "@mantine/core";
import { IconAlertCircle, IconInfoCircle } from "@tabler/icons-react";
import { type ReactNode, useState } from "react";
import type { BbErrorCode, Section } from "../../api/types";
import { tr } from "../../i18n";

/** What every tab of the bucket properties dialog receives. */
export interface TabProps<T> {
  accountId: string;
  bucket: string;
  /** The setting as it was last read. */
  section: Section<T>;
  /** Reads the setting again and shows what the bucket has now (after a save, or to retry). */
  reload: () => Promise<void>;
}

function failureTitle(code: BbErrorCode): string {
  switch (code) {
    case "auth":
      return tr("Access denied");
    case "not_found":
      return tr("Not found");
    default:
      return tr("This setting could not be read");
  }
}

/**
 * Shows a setting that could not be read: an error with a retry button, or a note that the server
 * does not offer it. A loaded setting is handed to `children`.
 */
export function SectionGate<T>({
  section,
  reload,
  children,
}: {
  section: Section<T>;
  reload: () => Promise<void>;
  children: (value: T) => ReactNode;
}) {
  const [retrying, setRetrying] = useState(false);
  if (section.type === "unsupported") {
    return (
      <Alert color="gray" icon={<IconInfoCircle size={16} />} role="note" title={tr("Not available")}>
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {section.message}
        </Text>
      </Alert>
    );
  }
  if (section.type === "failed") {
    return (
      <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" title={failureTitle(section.error.code)}>
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {section.error.message}
        </Text>
        <Button
          mt="xs"
          size="compact-xs"
          variant="default"
          loading={retrying}
          onClick={async () => {
            setRetrying(true);
            try {
              await reload();
            } finally {
              setRetrying(false);
            }
          }}
        >
          {tr("Try again")}
        </Button>
      </Alert>
    );
  }
  return <>{children(section.value)}</>;
}

/** The error of the last request of a tab, above its buttons. */
export function ActionError({ message }: { message: string | null }) {
  if (message === null) {
    return null;
  }
  return (
    <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" py="xs">
      <Text size="sm" style={{ overflowWrap: "anywhere" }}>
        {message}
      </Text>
    </Alert>
  );
}
