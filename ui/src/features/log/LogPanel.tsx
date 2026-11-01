import { Badge, Button, Chip, CloseButton, EmptyState, Group, Loader, Menu, Text, TextInput } from "@mantine/core";
import { IconChevronDown, IconFileExport, IconListDetails, IconSearch } from "@tabler/icons-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { useEffect, useRef } from "react";
import type { LogExportFormat, LogLevel } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { formatDateTime } from "../../lib/format";
import { LOG_LEVELS } from "../../state/log";
import type { AppModel } from "../../state/model";
import { togglePanel } from "../objects/actions";

const LOG_ROW_HEIGHT = 24;

const LEVEL_LABELS: Record<LogLevel, string> = { error: "Errors", warning: "Warnings", information: "Information" };
const LEVEL_BADGES: Record<LogLevel, { label: string; color: string }> = {
  error: { label: "Error", color: "red" },
  warning: { label: "Warning", color: "yellow" },
  information: { label: "Info", color: "blue" },
};

async function exportLog(model: AppModel, format: LogExportFormat): Promise<void> {
  try {
    const path = await model.platform.pickSavePath({
      title: "Export the event log",
      defaultPath: `bucketbridge-log.${format}`,
      filters: [{ name: format === "csv" ? "CSV" : "JSON", extensions: [format] }],
    });
    if (path === null) {
      return;
    }
    await model.api.export_log({ path, format });
    model.status.info(`Event log exported to ${path}.`);
  } catch (error) {
    model.status.error(error, { notify: "The event log could not be exported" });
  }
}

async function clearLog(model: AppModel): Promise<void> {
  const confirmed = await model.dialogs.confirm({
    title: "Clear the event log",
    message: "Remove all entries from the event log?",
    confirmLabel: "Clear log",
    danger: true,
  });
  if (!confirmed) {
    return;
  }
  try {
    await model.log.clear();
  } catch (error) {
    model.status.error(error, { notify: "The event log could not be cleared" });
  }
}

/** Bottom panel: filterable live event log with export and clear. */
export function LogPanel() {
  const model = useModel();
  const entries = useSlice(model.log.store, (state) => state.entries);
  const levels = useSlice(model.log.store, (state) => state.levels);
  const text = useSlice(model.log.store, (state) => state.text);
  const loading = useSlice(model.log.store, (state) => state.loading);
  const error = useSlice(model.log.store, (state) => state.error);
  const settingsLoaded = useSlice(model.settings.store, (state) => state.loaded);
  const scrollRef = useRef<HTMLDivElement>(null);
  const stickToBottom = useRef(true);
  const virtualizer = useVirtualizer({
    count: entries.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => LOG_ROW_HEIGHT,
    overscan: 10,
  });
  useEffect(() => {
    if (stickToBottom.current && entries.length > 0) {
      virtualizer.scrollToIndex(entries.length - 1, { align: "end" });
    }
  }, [entries.length, virtualizer]);
  const onScroll = () => {
    const element = scrollRef.current;
    if (element) {
      stickToBottom.current = element.scrollHeight - element.scrollTop - element.clientHeight < LOG_ROW_HEIGHT * 2;
    }
  };
  return (
    <section className="bb-panel bb-log-panel" aria-label="Event log">
      <Group className="bb-panel-header" justify="space-between" wrap="nowrap">
        <Group gap="sm" wrap="nowrap">
          <Group gap={6} wrap="nowrap">
            <IconListDetails size={16} aria-hidden />
            <Text size="sm" fw={600}>
              Event log
            </Text>
          </Group>
          <Chip.Group multiple value={levels} onChange={(value) => model.log.setLevels(value)}>
            <Group gap={4} wrap="nowrap" role="group" aria-label="Levels">
              {LOG_LEVELS.map((level) => (
                <Chip key={level} value={level} size="xs" variant="light">
                  {LEVEL_LABELS[level]}
                </Chip>
              ))}
            </Group>
          </Chip.Group>
          <TextInput
            size="xs"
            w={200}
            placeholder="Filter messages"
            aria-label="Filter messages"
            leftSection={<IconSearch size={14} />}
            value={text}
            onChange={(event) => model.log.setText(event.currentTarget.value)}
          />
          {loading ? <Loader size={14} aria-label="Loading log" /> : null}
        </Group>
        <Group gap={4} wrap="nowrap">
          <Menu position="bottom-end" withinPortal>
            <Menu.Target>
              <Button size="compact-xs" variant="subtle" leftSection={<IconFileExport size={14} />} rightSection={<IconChevronDown size={12} />}>
                Export
              </Button>
            </Menu.Target>
            <Menu.Dropdown>
              <Menu.Item onClick={() => void exportLog(model, "csv")}>Export as CSV...</Menu.Item>
              <Menu.Item onClick={() => void exportLog(model, "json")}>Export as JSON...</Menu.Item>
            </Menu.Dropdown>
          </Menu>
          <Button size="compact-xs" variant="subtle" color="red" onClick={() => void clearLog(model)}>
            Clear
          </Button>
          <CloseButton
            size="sm"
            aria-label="Hide event log"
            disabled={!settingsLoaded}
            onClick={() => void togglePanel(model, "log")}
          />
        </Group>
      </Group>
      <div ref={scrollRef} className="bb-panel-body" onScroll={onScroll}>
        {error !== null ? (
          <Text size="xs" c="red" p="xs" role="alert">
            {error}
          </Text>
        ) : null}
        {entries.length === 0 && !loading && error === null ? (
          <EmptyState size="xs" py="md" title="No log entries" description="Entries matching the filter appear here as they happen." />
        ) : (
          <div role="log" aria-label="Log entries" style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((item) => {
              const entry = entries[item.index];
              if (!entry) {
                return null;
              }
              const badge = LEVEL_BADGES[entry.level];
              return (
                <div
                  key={entry.id}
                  className="bb-log-row"
                  data-level={entry.level}
                  style={{ transform: `translateY(${item.start}px)`, height: LOG_ROW_HEIGHT }}
                >
                  <Text size="xs" c="dimmed" className="bb-log-time">
                    {formatDateTime(entry.ts)}
                  </Text>
                  <Badge size="xs" variant="light" color={badge.color} className="bb-log-level">
                    {badge.label}
                  </Badge>
                  <Text size="xs" c="dimmed" className="bb-log-source">
                    {entry.source}
                  </Text>
                  <Text size="xs" className="bb-ellipsis" title={entry.message}>
                    {entry.message}
                  </Text>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </section>
  );
}
