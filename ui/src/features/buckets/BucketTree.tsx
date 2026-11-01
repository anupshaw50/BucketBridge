import { ActionIcon, Alert, Badge, Button, Group, Loader, Skeleton, Stack, Text, Tooltip } from "@mantine/core";
import {
  IconAlertCircle,
  IconBucket,
  IconChevronDown,
  IconChevronRight,
  IconFolder,
  IconLock,
  IconPlugConnected,
  IconPlus,
  IconRefresh,
  IconX,
} from "@tabler/icons-react";
import { type KeyboardEvent, useRef, useState } from "react";
import type { Bucket, BucketKind } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { isMacPlatform } from "../../lib/keys";
import { folderEntry } from "../../lib/objects";
import { baseName, displaySegment } from "../../lib/s3keys";
import { type BrowserState, treeKey } from "../../state/browser";
import { removeExternalBucket, showAddExternalBucket } from "../bucketcfg/actions";
import { bucketsPaneContext } from "../menus/context";
import { ContextMenu } from "../menus/ContextMenu";
import { runAction } from "../menus/registry";
import { bucketMenu, bucketsEmptyMenu, itemsMenu } from "../menus/surfaces";
import { isContextClick, isMenuKey, useContextMenu } from "../menus/useContextMenu";

const KIND_LABELS: Record<BucketKind, string> = { aws: "AWS", external: "External", cloudfront: "CloudFront" };
const KIND_COLORS: Record<BucketKind, string> = { aws: "orange", external: "grape", cloudfront: "cyan" };

export function KindBadge({ kind }: { kind: BucketKind }) {
  return (
    <Badge size="xs" variant="light" color={KIND_COLORS[kind]} radius="sm" style={{ flexShrink: 0 }}>
      {KIND_LABELS[kind]}
    </Badge>
  );
}

type TreeRow =
  | { type: "bucket"; id: string; level: number; bucket: Bucket; expandable: boolean; expanded: boolean }
  | { type: "folder"; id: string; level: number; bucket: string; prefix: string; expandable: boolean; expanded: boolean }
  | { type: "loading"; id: string; level: number }
  | { type: "error"; id: string; level: number; bucket: string; prefix: string; message: string; more: boolean }
  | { type: "more"; id: string; level: number; bucket: string; prefix: string; busy: boolean };

function buildRows(state: BrowserState): TreeRow[] {
  const rows: TreeRow[] = [];
  const addChildren = (bucket: string, prefix: string, level: number) => {
    const key = treeKey(bucket, prefix);
    const node = state.tree[key];
    if (node === undefined || node.status === "loading" || node.status === "idle") {
      rows.push({ type: "loading", id: `${key}:loading`, level });
      return;
    }
    if (node.status === "error") {
      rows.push({
        type: "error",
        id: `${key}:error`,
        level,
        bucket,
        prefix,
        message: node.error ?? "Failed to load.",
        more: false,
      });
      return;
    }
    for (const child of node.prefixes) {
      const childKey = treeKey(bucket, child);
      const childNode = state.tree[childKey];
      const knownEmpty =
        childNode?.status === "ready" && childNode.prefixes.length === 0 && childNode.nextToken === null;
      const expanded = state.expanded[childKey] === true;
      rows.push({ type: "folder", id: childKey, level, bucket, prefix: child, expandable: !knownEmpty, expanded });
      if (expanded && !knownEmpty) {
        addChildren(bucket, child, level + 1);
      }
    }
    if (node.nextToken !== null) {
      rows.push({ type: "more", id: `${key}:more`, level, bucket, prefix, busy: node.loadingMore });
    }
    if (node.error !== null) {
      rows.push({ type: "error", id: `${key}:more-error`, level, bucket, prefix, message: node.error, more: true });
    }
  };
  for (const bucket of state.bucketList.buckets) {
    const key = treeKey(bucket.name, "");
    const node = state.tree[key];
    const knownEmpty = node?.status === "ready" && node.prefixes.length === 0 && node.nextToken === null;
    const expanded = state.expanded[key] === true;
    rows.push({ type: "bucket", id: key, level: 1, bucket, expandable: !knownEmpty, expanded });
    if (expanded && !knownEmpty) {
      addChildren(bucket.name, "", 2);
    }
  }
  return rows;
}

function BucketListState() {
  const model = useModel();
  const status = useSlice(model.browser.store, (state) => state.bucketList.status);
  const error = useSlice(model.browser.store, (state) => state.bucketList.error);
  const errorCode = useSlice(model.browser.store, (state) => state.bucketList.errorCode);
  const count = useSlice(model.browser.store, (state) => state.bucketList.buckets.length);
  const accountId = useSlice(model.browser.store, (state) => state.bucketList.accountId);
  if (status === "loading" && count === 0) {
    return (
      <Stack gap={8} p="sm" aria-busy="true" aria-label="Loading buckets">
        {Array.from({ length: 6 }, (_, index) => (
          <Skeleton key={index} height={16} width={`${85 - index * 8}%`} />
        ))}
      </Stack>
    );
  }
  if (status === "error") {
    const unlock = async () => {
      if (await model.prompts.requestUnlock()) {
        await model.browser.loadBuckets();
      }
    };
    return (
      <Alert color="red" m="xs" p="xs" icon={<IconAlertCircle size={16} />} role="alert" title="Buckets unavailable">
        <Text size="xs" style={{ overflowWrap: "anywhere" }}>
          {error}
        </Text>
        <Group gap={6} mt="xs">
          {errorCode === "locked" ? (
            <Button size="compact-xs" leftSection={<IconLock size={12} />} onClick={() => void unlock()}>
              Unlock
            </Button>
          ) : null}
          {errorCode === "auth" && accountId !== null ? (
            <Button
              size="compact-xs"
              variant="default"
              leftSection={<IconPlugConnected size={12} />}
              onClick={() => showAddExternalBucket(model)}
            >
              Add external bucket
            </Button>
          ) : null}
          <Button size="compact-xs" variant="default" onClick={() => void model.browser.loadBuckets()}>
            Try again
          </Button>
        </Group>
      </Alert>
    );
  }
  if (status === "ready" && count === 0 && accountId !== null) {
    return (
      <Stack gap="xs" p="sm" align="flex-start">
        <Text size="sm" c="dimmed">
          This account has no buckets yet.
        </Text>
        <Button
          size="compact-xs"
          leftSection={<IconPlus size={12} />}
          onClick={() => model.dialogs.open({ kind: "create-bucket", accountId })}
        >
          Create bucket
        </Button>
      </Stack>
    );
  }
  return null;
}

/** Left pane: the account's buckets with kind badges, and lazily loaded folders. */
export function BucketTree() {
  const model = useModel();
  const browser = useSlice(model.browser.store, (state) => state);
  const [focusedId, setFocusedId] = useState<string | null>(null);
  const rowRefs = useRef(new Map<string, HTMLDivElement>());
  const menu = useContextMenu();
  // Buckets picked together with Ctrl (Cmd on macOS) click; a menu on them acts on all of them.
  const [marked, setMarked] = useState<ReadonlySet<string>>(new Set());
  const markedBuckets = browser.bucketList.buckets.filter((bucket) => marked.has(bucket.name));
  const rows = buildRows(browser);
  const accountId = browser.bucketList.accountId;
  const current =
    browser.listing.bucket === null ? null : treeKey(browser.listing.bucket, browser.listing.prefix);
  const tabStop = rows.some((row) => row.id === focusedId) ? focusedId : (rows[0]?.id ?? null);

  const focusRow = (id: string) => {
    setFocusedId(id);
    rowRefs.current.get(id)?.focus();
  };
  const activate = (row: TreeRow) => {
    switch (row.type) {
      case "bucket":
        void model.browser.openLocation(row.bucket.name, "");
        break;
      case "folder":
        void model.browser.openLocation(row.bucket, row.prefix);
        break;
      case "more":
        void model.browser.loadTreeNode(row.bucket, row.prefix, true);
        break;
      case "error":
        void model.browser.loadTreeNode(row.bucket, row.prefix, row.more);
        break;
      case "loading":
        break;
    }
  };
  const toggleRow = (row: TreeRow) => {
    if (row.type === "bucket") {
      void model.browser.toggleNode(row.bucket.name, "");
    } else if (row.type === "folder") {
      void model.browser.toggleNode(row.bucket, row.prefix);
    }
  };
  /** The buckets a menu on this row acts on: the marked ones when the row is one of them, else just the row. */
  const targetsOf = (bucket: Bucket): Bucket[] =>
    marked.has(bucket.name) && markedBuckets.length > 1 ? markedBuckets : [bucket];
  const rowMenu = (row: TreeRow) => {
    if (row.type === "bucket") {
      return bucketMenu(model, targetsOf(row.bucket));
    }
    if (row.type === "folder") {
      return itemsMenu(model, row.bucket, [folderEntry(row.prefix)], "buckets");
    }
    return null;
  };
  const openRowMenu = (event: React.MouseEvent, row: TreeRow) => {
    const built = rowMenu(row) ?? bucketsEmptyMenu(model);
    if (row.type === "bucket" && !marked.has(row.bucket.name)) {
      setMarked(new Set());
    }
    menu.open(event, built.items, built.label);
  };
  const onRowClick = (event: React.MouseEvent, row: TreeRow) => {
    setFocusedId(row.id);
    if (isContextClick(event)) {
      openRowMenu(event, row);
      return;
    }
    // Ctrl (Cmd on macOS) click marks buckets without opening them.
    const additive = isMacPlatform() ? event.metaKey : event.ctrlKey || event.metaKey;
    if (row.type === "bucket" && additive) {
      const next = new Set(marked);
      if (next.has(row.bucket.name)) {
        next.delete(row.bucket.name);
      } else {
        next.add(row.bucket.name);
      }
      setMarked(next);
      return;
    }
    setMarked(new Set());
    activate(row);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    // Act on the row that has focus, which can be ahead of the focusedId state.
    const target = event.target instanceof HTMLElement ? event.target.closest("[data-tree-id]") : null;
    const activeId = target?.getAttribute("data-tree-id") ?? tabStop;
    const index = rows.findIndex((row) => row.id === activeId);
    const row = rows[index];
    if (row === undefined) {
      return;
    }
    const expandable = (row.type === "bucket" || row.type === "folder") && row.expandable;
    const expanded = (row.type === "bucket" || row.type === "folder") && row.expanded;
    switch (event.key) {
      case "ArrowDown": {
        const next = rows[index + 1];
        if (next) focusRow(next.id);
        break;
      }
      case "ArrowUp": {
        const previous = rows[index - 1];
        if (previous) focusRow(previous.id);
        break;
      }
      case "Home": {
        const first = rows[0];
        if (first) focusRow(first.id);
        break;
      }
      case "End": {
        const last = rows[rows.length - 1];
        if (last) focusRow(last.id);
        break;
      }
      case "ArrowRight":
        if (expandable && !expanded) {
          toggleRow(row);
        } else if (expanded) {
          const child = rows[index + 1];
          if (child && child.level > row.level) focusRow(child.id);
        }
        break;
      case "ArrowLeft":
        if (expanded) {
          toggleRow(row);
        } else {
          for (let back = index - 1; back >= 0; back -= 1) {
            const candidate = rows[back];
            if (candidate && candidate.level < row.level) {
              focusRow(candidate.id);
              break;
            }
          }
        }
        break;
      case "Enter":
      case " ":
        activate(row);
        break;
      case "F5":
        // F5 refreshes the pane that has focus: here, the list of buckets.
        runAction("bucket.refresh", bucketsPaneContext(model));
        break;
      case "Delete":
        if (row.type !== "bucket") {
          return;
        }
        runAction("bucket.delete", bucketsPaneContext(model, targetsOf(row.bucket)));
        break;
      default: {
        if (!isMenuKey(event)) {
          return;
        }
        const box = rowRefs.current.get(row.id)?.getBoundingClientRect();
        const built = rowMenu(row) ?? bucketsEmptyMenu(model);
        menu.openAt((box?.left ?? 0) + 24, box?.bottom ?? 0, built.items, built.label);
      }
    }
    event.preventDefault();
  };

  return (
    <div
      className="bb-tree-pane"
      onContextMenu={(event) => {
        const built = bucketsEmptyMenu(model);
        menu.open(event, built.items, built.label);
      }}
      onClick={(event) => {
        if (isContextClick(event)) {
          const built = bucketsEmptyMenu(model);
          menu.open(event, built.items, built.label);
        }
      }}
    >
      <Group className="bb-pane-header" justify="space-between" wrap="nowrap">
        <Text size="xs" fw={700} tt="uppercase" c="dimmed">
          Buckets
        </Text>
        <Group gap={2} wrap="nowrap">
          {browser.bucketList.status === "loading" && browser.bucketList.buckets.length > 0 ? (
            <Loader size={12} aria-label="Refreshing buckets" />
          ) : null}
          <Tooltip label="Add external bucket">
            <ActionIcon
              variant="subtle"
              size="sm"
              aria-label="Add external bucket"
              disabled={accountId === null}
              onClick={() => showAddExternalBucket(model)}
            >
              <IconPlugConnected size={14} />
            </ActionIcon>
          </Tooltip>
          <Tooltip label="New bucket">
            <ActionIcon
              variant="subtle"
              size="sm"
              aria-label="New bucket"
              disabled={accountId === null}
              onClick={() => {
                if (accountId !== null) {
                  model.dialogs.open({ kind: "create-bucket", accountId });
                }
              }}
            >
              <IconPlus size={14} />
            </ActionIcon>
          </Tooltip>
          <Tooltip label="Reload buckets">
            <ActionIcon
              variant="subtle"
              size="sm"
              aria-label="Reload buckets"
              disabled={accountId === null}
              onClick={() => {
                model.browser.resetTree();
                void model.browser.loadBuckets();
              }}
            >
              <IconRefresh size={14} />
            </ActionIcon>
          </Tooltip>
        </Group>
      </Group>
      <BucketListState />
      {rows.length > 0 ? (
        <div
          className="bb-tree"
          role="tree"
          aria-label="Buckets and folders"
          aria-multiselectable={markedBuckets.length > 0 || undefined}
          onKeyDown={onKeyDown}
        >
          {rows.map((row) => {
            const isItem = row.type === "bucket" || row.type === "folder";
            const isMarked = row.type === "bucket" && marked.has(row.bucket.name);
            const selected = isItem && (row.id === current || isMarked);
            return (
              <div
                key={row.id}
                ref={(element) => {
                  if (element) {
                    rowRefs.current.set(row.id, element);
                  } else {
                    rowRefs.current.delete(row.id);
                  }
                }}
                role="treeitem"
                data-tree-id={row.id}
                aria-level={row.level}
                aria-selected={isItem ? selected : undefined}
                aria-expanded={isItem && row.expandable ? row.expanded : undefined}
                tabIndex={row.id === tabStop ? 0 : -1}
                className="bb-tree-row"
                data-selected={selected || undefined}
                style={{ paddingLeft: 4 + (row.level - 1) * 14 }}
                onFocus={() => setFocusedId(row.id)}
                onClick={(event) => {
                  event.stopPropagation();
                  onRowClick(event, row);
                }}
                onContextMenu={(event) => openRowMenu(event, row)}
              >
                {isItem ? (
                  <span
                    className="bb-tree-toggle"
                    aria-hidden
                    onClick={(event) => {
                      event.stopPropagation();
                      if (row.expandable) {
                        toggleRow(row);
                      }
                    }}
                  >
                    {row.expandable ? (
                      row.expanded ? (
                        <IconChevronDown size={14} />
                      ) : (
                        <IconChevronRight size={14} />
                      )
                    ) : null}
                  </span>
                ) : (
                  <span className="bb-tree-toggle" aria-hidden />
                )}
                {row.type === "bucket" ? (
                  <>
                    <IconBucket size={15} aria-hidden className="bb-tree-icon" />
                    <span className="bb-ellipsis" title={`${row.bucket.name}${row.bucket.region ? ` (${row.bucket.region})` : ""}`}>
                      {row.bucket.name}
                    </span>
                    <KindBadge kind={row.bucket.kind} />
                    {accountId !== null && browser.bucketList.pinned.includes(row.bucket.name) ? (
                      <Tooltip label="Remove external bucket">
                        <ActionIcon
                          variant="subtle"
                          color="gray"
                          size="xs"
                          ml="auto"
                          tabIndex={-1}
                          aria-label={`Remove external bucket ${row.bucket.name}`}
                          onClick={(event) => {
                            event.stopPropagation();
                            void removeExternalBucket(model, accountId, row.bucket.name);
                          }}
                        >
                          <IconX size={12} />
                        </ActionIcon>
                      </Tooltip>
                    ) : null}
                  </>
                ) : null}
                {row.type === "folder" ? (
                  <>
                    <IconFolder size={15} aria-hidden className="bb-tree-icon" />
                    <span className="bb-ellipsis" title={row.prefix} data-empty-name={baseName(row.prefix) === "" || undefined}>
                      {displaySegment(baseName(row.prefix))}
                    </span>
                  </>
                ) : null}
                {row.type === "loading" ? (
                  <Text size="xs" c="dimmed">
                    Loading folders...
                  </Text>
                ) : null}
                {row.type === "more" ? (
                  <Text size="xs" c="blue">
                    {row.busy ? "Loading..." : "Load more folders"}
                  </Text>
                ) : null}
                {row.type === "error" ? (
                  <Text size="xs" c="red" title={row.message} className="bb-ellipsis">
                    Could not load folders. Select to retry.
                  </Text>
                ) : null}
              </div>
            );
          })}
        </div>
      ) : null}
      <ContextMenu menu={menu.state} onClose={menu.close} />
    </div>
  );
}
