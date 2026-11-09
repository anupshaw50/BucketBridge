import { ActionIcon, Group, Text, TextInput, Tooltip } from "@mantine/core";
import { IconPencil } from "@tabler/icons-react";
import { type ReactNode, useEffect, useRef, useState } from "react";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import { addressText } from "../../lib/address";

export const ADDRESS_INPUT_ID = "bb-address-input";

export interface AddressBarProps {
  bucket: string | null;
  prefix: string;
  /** The clickable breadcrumbs, shown while the address is not being edited. */
  children?: ReactNode;
}

/**
 * Address bar: breadcrumbs that turn into a text field. Typing or pasting `s3://bucket/folder/` (or
 * `bucket/folder`) and pressing Enter opens that location; a path that is not valid, or a bucket the
 * account cannot list, is explained under the field and the field stays open.
 */
export function AddressBar({ bucket, prefix, children }: AddressBarProps) {
  const model = useModel();
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const current = bucket === null ? "" : addressText(bucket, prefix);
  // With no bucket open there are no breadcrumbs, so the field is always there.
  const open = editing || bucket === null;

  const start = () => {
    setDraft(current);
    setError(null);
    setEditing(true);
  };
  const stop = () => {
    setEditing(false);
    setError(null);
  };

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && !event.shiftKey && !event.altKey && event.key.toLowerCase() === "l") {
        event.preventDefault();
        if (bucket === null) {
          inputRef.current?.focus();
        } else {
          setDraft(current);
          setError(null);
          setEditing(true);
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [bucket, current]);

  const submit = async () => {
    setBusy(true);
    const problem = await model.browser.goToAddress(draft);
    setBusy(false);
    if (problem === null) {
      setEditing(false);
      setDraft("");
      setError(null);
    } else {
      setError(problem);
    }
  };

  if (!open) {
    return (
      <Group gap={2} wrap="nowrap" style={{ minWidth: 0 }}>
        <nav aria-label={tr("Location")} style={{ minWidth: 0, overflow: "hidden" }}>
          {children}
        </nav>
        <Tooltip label={tr("Edit path (Ctrl+L)")}>
          <ActionIcon variant="subtle" size="sm" aria-label={tr("Edit path")} onClick={start}>
            <IconPencil size={14} />
          </ActionIcon>
        </Tooltip>
      </Group>
    );
  }
  return (
    <form
      style={{ flex: 1, minWidth: 0 }}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <TextInput
        id={ADDRESS_INPUT_ID}
        ref={inputRef}
        size="xs"
        aria-label={tr("Path")}
        placeholder={tr("s3://bucket/folder/")}
        value={draft}
        disabled={busy}
        error={error !== null}
        aria-invalid={error !== null || undefined}
        aria-describedby={error !== null ? `${ADDRESS_INPUT_ID}-error` : undefined}
        spellCheck={false}
        autoComplete="off"
        onChange={(event) => {
          setDraft(event.currentTarget.value);
          setError(null);
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            event.stopPropagation();
            stop();
          }
        }}
        onBlur={() => {
          if (error === null && !busy) {
            stop();
          }
        }}
      />
      {error !== null ? (
        <Text id={`${ADDRESS_INPUT_ID}-error`} size="xs" c="red" role="alert" style={{ overflowWrap: "anywhere" }}>
          {error}
        </Text>
      ) : null}
    </form>
  );
}
