import { ActionIcon, Badge, Button, Group, Stack, Text, TextInput, Tooltip, UnstyledButton } from "@mantine/core";
import { IconArrowUp, IconFile, IconFolder } from "@tabler/icons-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useMemo, useRef, useState, useSyncExternalStore } from "react";
import { useStore } from "zustand";
import { AppModal } from "../components/AppModal";
import { formatBytes, formatDateTime } from "../lib/format";
import type { MockPicker, PickRequest } from "./browserPlatform";
import { dirname, joinPath } from "./fs";

const ACTION_LABELS = {
  files: "Open",
  folder: "Choose this folder",
  save: "Save",
  reveal: "Close",
} as const;

function ChooserBody({ picker, request }: { picker: MockPicker; request: PickRequest }) {
  const [dir, setDir] = useState(request.startDir);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [fileName, setFileName] = useState(request.fileName);
  const version = useSyncExternalStore(
    (listener) => picker.fs.subscribe(listener),
    () => picker.fs.version,
  );
  const entries = useMemo(() => (version >= 0 ? picker.fs.readDir(dir) : []), [picker, dir, version]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 30,
    overscan: 10,
  });
  const enter = (path: string) => {
    setDir(path);
    setSelected(new Set());
  };
  const toggle = (path: string) => {
    const next = new Set(selected);
    if (next.has(path)) {
      next.delete(path);
    } else {
      next.add(path);
    }
    setSelected(next);
  };
  const trimmedName = fileName.trim();
  const nameError = trimmedName.includes("/") ? "A file name cannot contain a slash." : null;
  const canConfirm =
    request.mode === "files"
      ? selected.size > 0
      : request.mode === "save"
        ? trimmedName.length > 0 && nameError === null
        : true;
  const confirm = () => {
    switch (request.mode) {
      case "files":
        picker.finish(entries.filter((entry) => selected.has(entry.path)).map((entry) => entry.path));
        break;
      case "folder":
        picker.finish([dir]);
        break;
      case "save":
        picker.finish([joinPath(dir, trimmedName)]);
        break;
      case "reveal":
        picker.finish(null);
        break;
    }
  };
  return (
    <Stack gap="xs">
      <Group gap="xs" wrap="nowrap">
        <Tooltip label="Parent folder">
          <ActionIcon variant="default" aria-label="Parent folder" disabled={dir === "/"} onClick={() => enter(dirname(dir))}>
            <IconArrowUp size={16} />
          </ActionIcon>
        </Tooltip>
        <Text size="sm" ff="monospace" style={{ overflowWrap: "anywhere" }} aria-label="Current folder">
          {dir}
        </Text>
      </Group>
      <div
        ref={scrollRef}
        style={{
          height: 280,
          overflow: "auto",
          border: "1px solid var(--mantine-color-default-border)",
          borderRadius: 4,
        }}
      >
        {entries.length === 0 ? (
          <Text size="sm" c="dimmed" p="sm">
            This folder is empty.
          </Text>
        ) : null}
        <div role="list" aria-label="Folder contents" style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
          {virtualizer.getVirtualItems().map((item) => {
            const entry = entries[item.index];
            if (!entry) {
              return null;
            }
            const isSelected = selected.has(entry.path) || entry.path === request.highlight;
            const disabled = entry.kind === "file" && request.mode === "folder";
            return (
              <UnstyledButton
                key={entry.path}
                role="listitem"
                aria-pressed={entry.kind === "file" && request.mode === "files" ? isSelected : undefined}
                disabled={disabled}
                onClick={() => {
                  if (entry.kind === "dir") {
                    enter(entry.path);
                  } else if (request.mode === "files") {
                    toggle(entry.path);
                  } else if (request.mode === "save") {
                    setFileName(entry.name);
                  }
                }}
                px="sm"
                style={{
                  position: "absolute",
                  top: 0,
                  left: 0,
                  right: 0,
                  height: 30,
                  transform: `translateY(${item.start}px)`,
                  display: "flex",
                  gap: 8,
                  alignItems: "center",
                  opacity: disabled ? 0.5 : 1,
                  background: isSelected ? "var(--mantine-primary-color-light)" : undefined,
                }}
              >
                {entry.kind === "dir" ? <IconFolder size={16} /> : <IconFile size={16} />}
                <Text size="sm" style={{ flex: 1, overflowWrap: "anywhere" }}>
                  {entry.name}
                </Text>
                {entry.kind === "file" ? (
                  <Text size="xs" c="dimmed">
                    {formatBytes(entry.size)} · {formatDateTime(entry.modified)}
                  </Text>
                ) : null}
              </UnstyledButton>
            );
          })}
        </div>
      </div>
      {request.mode === "save" ? (
        <TextInput
          label="File name"
          value={fileName}
          onChange={(event) => setFileName(event.currentTarget.value)}
          error={nameError}
        />
      ) : null}
      <Group justify="space-between">
        <Text size="xs" c="dimmed">
          {request.mode === "files" ? `${selected.size} selected` : ""}
        </Text>
        <Group gap="xs">
          {request.mode !== "reveal" ? (
            <Button variant="default" onClick={() => picker.finish(null)}>
              Cancel
            </Button>
          ) : null}
          <Button onClick={confirm} disabled={!canConfirm}>
            {ACTION_LABELS[request.mode]}
          </Button>
        </Group>
      </Group>
    </Stack>
  );
}

/** In-app chooser over the simulated file system; only used outside Tauri. */
export function MockFileChooser({ picker }: { picker: MockPicker }) {
  const request = useStore(picker.store, (state) => state.request);
  return (
    <AppModal
      opened={request !== null}
      onClose={() => picker.finish(null)}
      size="lg"
      title={
        <Group gap="xs">
          <span>{request?.title ?? ""}</span>
          <Badge size="xs" variant="light" color="gray">
            Simulated file system
          </Badge>
        </Group>
      }
    >
      {request !== null ? <ChooserBody key={`${request.mode}:${request.startDir}`} picker={picker} request={request} /> : null}
    </AppModal>
  );
}
