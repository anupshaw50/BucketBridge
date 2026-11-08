import {
  ActionIcon,
  Alert,
  Anchor,
  Breadcrumbs,
  Button,
  CloseButton,
  EmptyState,
  Group,
  Loader,
  Skeleton,
  Stack,
  Text,
  TextInput,
  Tooltip,
} from "@mantine/core";
import {
  IconAlertCircle,
  IconArrowUp,
  IconBucket,
  IconFolderOpen,
  IconLock,
  IconSearch,
  IconUpload,
} from "@tabler/icons-react";
import { useEffect, useRef } from "react";
import { useModel, useSlice } from "../../app/context";
import { formatCount } from "../../lib/format";
import { tr } from "../../i18n";
import { breadcrumbs, displaySegment } from "../../lib/s3keys";
import { ActiveFilterChip } from "../tools/FilterControls";
import { SEARCH_INPUT_ID, uploadFiles, uploadFolder } from "./actions";
import { OBJECT_ROW_HEIGHT, ObjectTable } from "./ObjectTable";

function LocationBar() {
  const model = useModel();
  const bucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const prefix = useSlice(model.browser.store, (state) => state.listing.prefix);
  const flat = useSlice(model.browser.store, (state) => state.listing.flat);
  const search = useSlice(model.browser.store, (state) => state.search);
  const filtered = useSlice(model.browser.store, (state) => state.filter !== null);
  const refreshing = useSlice(model.browser.store, (state) => state.listing.refreshing);
  const loaded = useSlice(model.browser.store, (state) => state.listing.entries.length);
  const shown = useSlice(model.browser.store, (state) => state.visible.length);
  if (bucket === null) {
    return null;
  }
  const crumbs = breadcrumbs(prefix);
  return (
    <Group className="bb-location" gap="xs" wrap="nowrap" justify="space-between">
      <Group gap={6} wrap="nowrap" style={{ minWidth: 0, flex: 1 }}>
        <Tooltip label={tr("Parent folder (Backspace)")}>
          <ActionIcon
            variant="subtle"
            size="sm"
            aria-label={tr("Parent folder")}
            disabled={prefix === ""}
            onClick={() => model.browser.goUp()}
          >
            <IconArrowUp size={16} />
          </ActionIcon>
        </Tooltip>
        <nav aria-label={tr("Location")} style={{ minWidth: 0, overflow: "hidden" }}>
          <Breadcrumbs separatorMargin={4} styles={{ root: { flexWrap: "nowrap" } }}>
            {[
              <Anchor
                key="bucket"
                component="button"
                type="button"
                size="sm"
                onClick={() => void model.browser.openLocation(bucket, "")}
                aria-current={prefix === "" ? "location" : undefined}
              >
                <Group gap={4} wrap="nowrap">
                  <IconBucket size={14} aria-hidden />
                  {bucket}
                </Group>
              </Anchor>,
              ...crumbs.map((crumb, index) => (
                <Anchor
                  key={crumb.prefix}
                  component="button"
                  type="button"
                  size="sm"
                  onClick={() => void model.browser.openLocation(bucket, crumb.prefix)}
                  aria-current={index === crumbs.length - 1 ? "location" : undefined}
                  fs={crumb.name === "" ? "italic" : undefined}
                >
                  {displaySegment(crumb.name)}
                </Anchor>
              )),
            ]}
          </Breadcrumbs>
        </nav>
        {flat ? (
          <Text size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }}>
            {tr("(flat view)")}
          </Text>
        ) : null}
        {refreshing ? <Loader size={14} aria-label={tr("Refreshing")} /> : null}
      </Group>
      <Group gap="xs" wrap="nowrap">
        <ActiveFilterChip />
        <Text size="xs" c="dimmed" style={{ whiteSpace: "nowrap" }} aria-live="polite">
          {search.trim().length > 0 || filtered
            ? tr("Loaded {shown} of {loaded}", { shown: formatCount(shown), loaded: formatCount(loaded) })
            : loaded === 1
              ? tr("1 item")
              : tr("{n} items", { n: formatCount(loaded) })}
        </Text>
        <TextInput
          id={SEARCH_INPUT_ID}
          size="xs"
          w={220}
          placeholder={tr("Filter loaded items")}
          aria-label={tr("Filter loaded items")}
          leftSection={<IconSearch size={14} />}
          value={search}
          onChange={(event) => model.browser.setSearch(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key === "Escape" && search.length > 0) {
              event.stopPropagation();
              model.browser.setSearch("");
            }
          }}
          rightSection={
            search.length > 0 ? (
              <CloseButton size="sm" aria-label={tr("Clear filter")} onClick={() => model.browser.setSearch("")} />
            ) : null
          }
        />
      </Group>
    </Group>
  );
}

function ListSkeleton() {
  return (
    <>
      <span className="bb-visually-hidden">{tr("Loading objects")}</span>
      <Stack gap={6} p="sm" aria-hidden>
        {Array.from({ length: 10 }, (_, index) => (
          <Skeleton key={index} height={OBJECT_ROW_HEIGHT - 8} radius="sm" width={`${90 - (index % 4) * 12}%`} />
        ))}
      </Stack>
    </>
  );
}

function ListError() {
  const model = useModel();
  const listing = useSlice(model.browser.store, (state) => state.listing);
  const retryRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    // The object list had focus and was replaced by this message: keep keyboard users here.
    if (document.activeElement === null || document.activeElement === document.body) {
      retryRef.current?.focus();
    }
  }, []);
  const bucket = listing.bucket;
  if (bucket === null) {
    return null;
  }
  const retry = () => void model.browser.openLocation(bucket, listing.prefix);
  const unlockAndRetry = async () => {
    if (await model.prompts.requestUnlock()) {
      retry();
    }
  };
  return (
    <Stack p="md">
      <Alert color="red" icon={<IconAlertCircle size={16} />} title={tr("The folder could not be listed")} role="alert">
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {listing.error}
        </Text>
        <Group gap="xs" mt="sm">
          {listing.errorCode === "locked" ? (
            <Button size="xs" leftSection={<IconLock size={14} />} onClick={() => void unlockAndRetry()}>
              {tr("Unlock")}
            </Button>
          ) : null}
          <Button ref={retryRef} size="xs" variant="default" onClick={retry}>
            {tr("Try again")}
          </Button>
        </Group>
      </Alert>
    </Stack>
  );
}

function EmptyFolder() {
  const model = useModel();
  return (
    <EmptyState
      icon={<IconFolderOpen size={28} />}
      title={tr("This folder is empty")}
      description={tr("Upload files or a folder, or paste items you copied.")}
      size="sm"
      py="xl"
    >
      <Group gap="xs" justify="center" mt="sm">
        <Button size="xs" leftSection={<IconUpload size={14} />} onClick={() => void uploadFiles(model)}>
          {tr("Upload files")}
        </Button>
        <Button size="xs" variant="default" onClick={() => void uploadFolder(model)}>
          {tr("Upload folder")}
        </Button>
      </Group>
    </EmptyState>
  );
}

function LoadMoreBar() {
  const model = useModel();
  const nextToken = useSlice(model.browser.store, (state) => state.listing.nextToken);
  const loadingMore = useSlice(model.browser.store, (state) => state.listing.loadingMore);
  const refreshing = useSlice(model.browser.store, (state) => state.listing.refreshing);
  const moreError = useSlice(model.browser.store, (state) => state.listing.moreError);
  if (nextToken === null && moreError === null) {
    return null;
  }
  return (
    <Group className="bb-load-more" justify="center" gap="sm">
      {moreError !== null ? (
        <Text size="xs" c="red" role="alert">
          {moreError}
        </Text>
      ) : (
        <Text size="xs" c="dimmed">
          {tr("More items are available in this folder.")}
        </Text>
      )}
      <Button
        size="compact-xs"
        variant="light"
        loading={loadingMore}
        disabled={refreshing}
        onClick={() => void model.browser.loadMore()}
      >
        {moreError !== null ? tr("Try again") : tr("Load more")}
      </Button>
    </Group>
  );
}

/** Right-hand pane: location bar, object table and its loading, empty and error states. */
export function ObjectBrowser() {
  const model = useModel();
  const bucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const status = useSlice(model.browser.store, (state) => state.listing.status);
  const loaded = useSlice(model.browser.store, (state) => state.listing.entries.length);
  const shown = useSlice(model.browser.store, (state) => state.visible.length);
  const search = useSlice(model.browser.store, (state) => state.search);
  const filterName = useSlice(model.browser.store, (state) => state.filter?.name ?? null);
  if (bucket === null) {
    return (
      <EmptyState
        icon={<IconBucket size={28} />}
        title={tr("No bucket open")}
        description={tr("Choose a bucket on the left to see its contents.")}
        size="sm"
        py="xl"
      />
    );
  }
  // The table stays mounted while a folder loads, is empty or has no matches,
  // so keyboard focus survives folder navigation.
  let placeholder: React.ReactNode = null;
  if (status === "loading" || status === "idle") {
    placeholder = <ListSkeleton />;
  } else if (loaded === 0) {
    placeholder = <EmptyFolder />;
  } else if (shown === 0) {
    placeholder = (
      <EmptyState
        icon={<IconSearch size={28} />}
        title={tr("No matches")}
        description={
          search.trim().length === 0 && filterName !== null
            ? tr("None of the loaded items match the saved filter \"{name}\".", { name: filterName })
            : tr("None of the loaded items match \"{name}\".", { name: search.trim() })
        }
        size="sm"
        py="xl"
      />
    );
  }
  const body =
    status === "error" ? (
      <ListError />
    ) : (
      <ObjectTable placeholder={placeholder} busy={status === "loading" || status === "idle"} />
    );
  return (
    <section className="bb-browser" aria-label={tr("Bucket contents")}>
      <LocationBar />
      <div className="bb-browser-body">{body}</div>
      {status === "ready" ? <LoadMoreBar /> : null}
    </section>
  );
}
