import {
  ActionIcon,
  Badge,
  Button,
  CloseButton,
  EmptyState,
  Group,
  Progress,
  SegmentedControl,
  Text,
  Tooltip,
} from "@mantine/core";
import {
  IconArrowsMove,
  IconCopy,
  IconDownload,
  IconPlayerPause,
  IconPlayerPlay,
  IconRefresh,
  IconTrash,
  IconTransfer,
  IconUpload,
  IconX,
} from "@tabler/icons-react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { memo, useMemo, useSyncExternalStore, useRef, useState } from "react";
import type { TransferKind, TransferState, TransferTask } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { getLocale, subscribeLocale, tr } from "../../i18n";
import { formatCount, formatEta, formatSpeed, percent } from "../../lib/format";
import {
  canCancel,
  canPause,
  canResume,
  canRetry,
  describeLocation,
  inView,
  KIND_LABELS,
  progressText,
  STATE_LABELS,
  TRANSFER_VIEWS,
  type TransferView,
} from "../../lib/transfers";
import type { AppModel } from "../../state/model";
import { togglePanel } from "../objects/actions";

export const TRANSFER_ROW_HEIGHT = 46;

const KIND_ICONS: Record<TransferKind, typeof IconUpload> = {
  upload: IconUpload,
  download: IconDownload,
  copy: IconCopy,
  move: IconArrowsMove,
  delete: IconTrash,
};

const STATE_COLORS: Record<TransferState, string> = {
  queued: "gray",
  running: "blue",
  stopped: "yellow",
  completed: "teal",
  failed: "red",
  cancelled: "gray",
  skipped: "orange",
};

const VIEW_LABELS: Record<TransferView, string> = {
  all: tr("All"),
  queued: tr("Queued"),
  running: tr("Running"),
  stopped: tr("Stopped"),
  failed: tr("Failed"),
};

/** Titles of the empty panel per view. */
const EMPTY_TITLES: Record<TransferView, string> = {
  all: tr("No transfers"),
  queued: tr("No queued transfers"),
  running: tr("No running transfers"),
  stopped: tr("No stopped transfers"),
  failed: tr("No failed transfers"),
};

type TaskAction = "pause" | "resume" | "cancel" | "retry";

async function runTaskAction(model: AppModel, action: TaskAction, id: string): Promise<void> {
  try {
    switch (action) {
      case "pause":
        await model.api.pause_task({ id });
        break;
      case "resume":
        await model.api.resume_task({ id });
        break;
      case "cancel":
        await model.api.cancel_task({ id });
        break;
      case "retry":
        await model.api.retry_task({ id });
        break;
    }
  } catch (error) {
    model.status.error(error, { notify: tr("The transfer could not be changed") });
  }
}

interface RowProps {
  task: TransferTask;
  /** The task waits for an answer to an overwrite question. */
  waiting: boolean;
  top: number;
  onAction: (action: TaskAction, id: string) => void;
}

const TransferRow = memo(function TransferRow({ task, waiting, top, onAction }: RowProps) {
  // The row is memoised, so it subscribes to the language itself.
  useSyncExternalStore(subscribeLocale, getLocale);
  const Icon = KIND_ICONS[task.kind];
  const source = describeLocation(task.src);
  const destination = describeLocation(task.dst);
  const value = task.state === "completed" ? 100 : (percent(task.bytes_done, task.bytes_total) ?? 0);
  const label =
    destination !== ""
      ? tr("{kind} {src} to {dst}", { kind: KIND_LABELS[task.kind], src: source, dst: destination })
      : tr("{kind} {src}", { kind: KIND_LABELS[task.kind], src: source });
  return (
    <div
      role="listitem"
      aria-label={label}
      className="bb-task"
      data-state={task.state}
      style={{ transform: `translateY(${top}px)`, height: TRANSFER_ROW_HEIGHT }}
    >
      <Tooltip label={KIND_LABELS[task.kind]}>
        <Icon size={16} className="bb-task-icon" aria-label={KIND_LABELS[task.kind]} />
      </Tooltip>
      <div className="bb-task-main">
        <Text size="xs" className="bb-ellipsis" title={label}>
          <span>{source}</span>
          {destination ? (
            <>
              <span className="bb-task-arrow" aria-hidden>
                {" → "}
              </span>
              <span>{destination}</span>
            </>
          ) : null}
        </Text>
        <Group gap="xs" wrap="nowrap">
          <Progress
            value={value}
            size="sm"
            w={140}
            color={STATE_COLORS[task.state]}
            aria-label={tr("Progress {percent}%", { percent: Math.round(value) })}
            animated={task.state === "running"}
          />
          <Text size="xs" c="dimmed" className="bb-task-meta">
            {progressText(task)}
            {task.state === "running" && task.speed > 0 ? ` · ${formatSpeed(task.speed)}` : ""}
            {task.state === "running" && task.eta !== null ? ` · ${tr("{eta} left", { eta: formatEta(task.eta) })}` : ""}
          </Text>
          {task.error ? (
            <Text size="xs" c={task.state === "skipped" ? "dimmed" : "red"} className="bb-ellipsis" title={task.error}>
              {task.state === "skipped" ? tr("Skipped: {reason}", { reason: task.error }) : task.error}
            </Text>
          ) : null}
        </Group>
      </div>
      <Badge size="sm" variant="light" color={waiting ? "orange" : STATE_COLORS[task.state]} className="bb-task-state">
        {waiting ? tr("Waiting") : STATE_LABELS[task.state]}
      </Badge>
      <Group gap={2} wrap="nowrap" className="bb-task-actions">
        <Tooltip label={tr("Pause")}>
          <ActionIcon
            size="sm"
            variant="subtle"
            aria-label={tr("Pause transfer")}
            disabled={!canPause(task.state)}
            onClick={() => onAction("pause", task.id)}
          >
            <IconPlayerPause size={14} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={tr("Resume")}>
          <ActionIcon
            size="sm"
            variant="subtle"
            aria-label={tr("Resume transfer")}
            disabled={!canResume(task.state)}
            onClick={() => onAction("resume", task.id)}
          >
            <IconPlayerPlay size={14} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={tr("Retry")}>
          <ActionIcon
            size="sm"
            variant="subtle"
            aria-label={tr("Retry transfer")}
            disabled={!canRetry(task.state)}
            onClick={() => onAction("retry", task.id)}
          >
            <IconRefresh size={14} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={tr("Cancel")}>
          <ActionIcon
            size="sm"
            variant="subtle"
            color="red"
            aria-label={tr("Cancel transfer")}
            disabled={!canCancel(task.state)}
            onClick={() => onAction("cancel", task.id)}
          >
            <IconX size={14} />
          </ActionIcon>
        </Tooltip>
      </Group>
    </div>
  );
});

/** Bottom panel: every transfer task, with views, bulk actions and per-task controls. */
export function TransferPanel() {
  const model = useModel();
  const [view, setView] = useState<TransferView>("all");
  const order = useSlice(model.transfers.store, (state) => state.order);
  const tasks = useSlice(model.transfers.store, (state) => state.tasks);
  const counts = useSlice(model.transfers.store, (state) => state.counts);
  const settingsLoaded = useSlice(model.settings.store, (state) => state.loaded);
  const questions = useSlice(model.conflicts.store, (state) => state.pending);
  const waitingIds = useMemo(() => new Set(questions.map((item) => item.task_id)), [questions]);
  const ids = useMemo(
    () => (view === "all" ? order : order.filter((id) => inView(view, tasks.get(id)?.state ?? "cancelled"))),
    [order, tasks, view],
  );
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: ids.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => TRANSFER_ROW_HEIGHT,
    overscan: 8,
  });
  const onAction = useMemo(
    () => (action: TaskAction, id: string) => {
      void runTaskAction(model, action, id);
    },
    [model],
  );
  const bulk = async (action: "start" | "stop" | "cancel" | "clear") => {
    try {
      if (action === "start") {
        await model.api.start_all();
      } else if (action === "stop") {
        await model.api.stop_all();
      } else if (action === "clear") {
        await model.api.clear_finished();
      } else {
        const unfinished = counts.queued + counts.running + counts.stopped + counts.failed;
        const confirmed = await model.dialogs.confirm({
          title: tr("Cancel all transfers"),
          message:
            unfinished === 1
              ? tr("Cancel 1 unfinished transfer? Partly transferred files and objects are removed.")
              : tr("Cancel {count} unfinished transfers? Partly transferred files and objects are removed.", {
                  count: formatCount(unfinished),
                }),
          confirmLabel: tr("Cancel transfers"),
          danger: true,
        });
        if (confirmed) {
          await model.api.cancel_all();
        }
      }
    } catch (error) {
      model.status.error(error, { notify: tr("The transfer queue could not be changed") });
    }
  };
  const unfinished = counts.queued + counts.running + counts.stopped + counts.failed;
  return (
    <section className="bb-panel bb-transfer-panel" aria-label={tr("Transfer queue")}>
      <Group className="bb-panel-header" justify="space-between" wrap="nowrap">
        <Group gap="sm" wrap="nowrap">
          <Group gap={6} wrap="nowrap">
            <IconTransfer size={16} aria-hidden />
            <Text size="sm" fw={600}>
              {tr("Transfers")}
            </Text>
          </Group>
          <SegmentedControl
            size="xs"
            value={view}
            onChange={setView}
            aria-label={tr("Transfer view")}
            data={TRANSFER_VIEWS.map((item) => ({
              value: item,
              label: `${VIEW_LABELS[item]} ${formatCount(counts[item])}`,
            }))}
          />
        </Group>
        <Group gap={4} wrap="nowrap">
          <Button size="compact-xs" variant="subtle" disabled={counts.stopped === 0} onClick={() => void bulk("start")}>
            {tr("Start all")}
          </Button>
          <Button
            size="compact-xs"
            variant="subtle"
            disabled={counts.queued + counts.running === 0}
            onClick={() => void bulk("stop")}
          >
            {tr("Stop all")}
          </Button>
          <Button
            size="compact-xs"
            variant="subtle"
            color="red"
            disabled={unfinished === 0}
            onClick={() => void bulk("cancel")}
          >
            {tr("Cancel all")}
          </Button>
          <Button
            size="compact-xs"
            variant="subtle"
            disabled={counts.completed + counts.cancelled + counts.skipped === 0}
            onClick={() => void bulk("clear")}
          >
            {tr("Clear finished")}
          </Button>
          <CloseButton
            size="sm"
            aria-label={tr("Hide transfers")}
            disabled={!settingsLoaded}
            onClick={() => void togglePanel(model, "transfers")}
          />
        </Group>
      </Group>
      <div ref={scrollRef} className="bb-panel-body">
        {ids.length === 0 ? (
          <EmptyState
            size="xs"
            py="md"
            title={EMPTY_TITLES[view]}
            description={view === "all" ? tr("Uploads, downloads, copies, moves and deletes appear here.") : undefined}
          />
        ) : (
          <div role="list" aria-label={tr("Transfers")} style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
            {virtualizer.getVirtualItems().map((item) => {
              const id = ids[item.index];
              const task = id === undefined ? undefined : tasks.get(id);
              return task ? (
                <TransferRow key={task.id} task={task} waiting={waitingIds.has(task.id)} top={item.start} onAction={onAction} />
              ) : null;
            })}
          </div>
        )}
      </div>
    </section>
  );
}
