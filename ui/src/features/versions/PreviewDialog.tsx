import { ActionIcon, Alert, Badge, Button, Group, Loader, NumberInput, Select, Stack, Text, Tooltip } from "@mantine/core";
import {
  IconAlertCircle,
  IconBraces,
  IconChevronLeft,
  IconChevronRight,
  IconChevronsLeft,
  IconChevronsRight,
  IconFile,
  IconFileText,
  IconPhoto,
  IconTable,
} from "@tabler/icons-react";
import { type KeyboardEvent, useEffect, useMemo, useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type { ObjectPreview, PreviewKind } from "../../api/types";
import { useModel } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { tr } from "../../i18n";
import { formatBytes, formatCount, formatDateTime } from "../../lib/format";
import { baseName, s3Uri } from "../../lib/s3keys";
import { ImageViewer } from "./ImageViewer";
import { imageSource } from "./imageUrl";
import { PreviewTableView } from "./PreviewTableView";

export interface PreviewDialogProps {
  accountId: string;
  bucket: string;
  objectKey: string;
  /** The version to show; `null` is the current one. */
  versionId: string | null;
  onClose: () => void;
}

const DEFAULT_PAGE_SIZE = 131_072;
const PARQUET_TYPE = "application/vnd.apache.parquet";
const PAGE_SIZES = [
  { value: "65536", label: "64 KiB" },
  { value: "131072", label: "128 KiB" },
  { value: "262144", label: "256 KiB" },
  { value: "524288", label: "512 KiB" },
  { value: "1048576", label: "1 MiB" },
];

interface Loaded {
  id: number;
  preview: ObjectPreview | null;
  error: string | null;
}

function KindIcon({ kind }: { kind: PreviewKind | null }) {
  switch (kind) {
    case "image":
    case "too_large":
      return <IconPhoto size={18} aria-hidden />;
    case "json":
      return <IconBraces size={18} aria-hidden />;
    case "table":
      return <IconTable size={18} aria-hidden />;
    case "text":
      return <IconFileText size={18} aria-hidden />;
    case "binary":
    case "empty":
    case null:
      return <IconFile size={18} aria-hidden />;
  }
}

/** Shows one object, or one version of it: an image, text or JSON (page by page), or why there is nothing to show. */
export function PreviewDialog({ accountId, bucket, objectKey, versionId, onClose }: PreviewDialogProps) {
  const model = useModel();
  const [request, setRequest] = useState({ id: 0, page: 0, pageSize: DEFAULT_PAGE_SIZE });
  const [sizeChoice, setSizeChoice] = useState(String(DEFAULT_PAGE_SIZE));
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [brokenImage, setBrokenImage] = useState<string | null>(null);
  const typedPage = useRef<number | "">("");

  useEffect(() => {
    let active = true;
    model.api
      .preview_object({
        account_id: accountId,
        bucket,
        key: objectKey,
        version_id: versionId,
        page: request.page,
        page_size: request.pageSize,
      })
      .then(
        (preview) => {
          if (active) {
            setLoaded({ id: request.id, preview, error: null });
          }
        },
        (raw: unknown) => {
          if (active) {
            setLoaded({ id: request.id, preview: null, error: toBbError(raw).message });
          }
        },
      );
    return () => {
      active = false;
    };
  }, [model, accountId, bucket, objectKey, versionId, request]);

  // While the next page loads, the last answer stays on screen (and keeps the focus).
  const loading = loaded?.id !== request.id;
  const current = loaded;
  const preview = current?.preview ?? null;
  const image = preview?.kind === "image" ? preview : null;
  const imageBase64 = image?.image_base64 ?? null;
  const imageType = image?.media_type ?? null;
  const source = useMemo(
    () => (imageBase64 !== null && imageType !== null ? imageSource(imageBase64, imageType) : null),
    [imageBase64, imageType],
  );
  useEffect(() => () => source?.revoke(), [source]);

  const goTo = (page: number) => {
    if (preview !== null && !loading) {
      const clamped = Math.max(0, Math.min(preview.page_count - 1, Math.trunc(page)));
      setRequest((r) => ({ id: r.id + 1, page: clamped, pageSize: r.pageSize }));
    }
  };
  const applySize = () => setRequest((r) => ({ id: r.id + 1, page: 0, pageSize: Number(sizeChoice) }));
  const pageable = preview !== null && (preview.kind === "text" || preview.kind === "json" || preview.kind === "table");
  const paged = pageable && preview.page_count > 1 && !loading;
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!paged || !event.altKey) {
      return;
    }
    const page = preview.page;
    const target =
      event.key === "ArrowLeft"
        ? page - 1
        : event.key === "ArrowRight"
          ? page + 1
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? preview.page_count - 1
              : null;
    if (target !== null) {
      event.preventDefault();
      goTo(target);
    }
  };

  let body: React.ReactNode;
  if (current === null) {
    body = (
      <Group justify="center" p="xl">
        <Loader size="sm" aria-label={tr("Loading preview")} />
      </Group>
    );
  } else if (preview === null) {
    body = (
      <Alert color="red" icon={<IconAlertCircle size={16} />} title={tr("The preview could not be loaded")} role="alert">
        <Text size="sm" style={{ overflowWrap: "anywhere" }}>
          {current.error}
        </Text>
      </Alert>
    );
  } else if (preview.kind === "text" || preview.kind === "json") {
    body = (
      <pre className="bb-preview-text" tabIndex={0} aria-label={tr("Contents of {name}", { name: baseName(objectKey) })}>
        {preview.text}
      </pre>
    );
  } else if (preview.kind === "table" && preview.table !== null) {
    body = <PreviewTableView table={preview.table} offset={preview.offset} label={tr("Contents of {name}", { name: baseName(objectKey) })} />;
  } else if (preview.kind === "image") {
    body =
      source === null || brokenImage === source.url ? (
        <Alert color="yellow" icon={<IconAlertCircle size={16} />} title={tr("The image cannot be shown")} role="alert">
          {tr("This system cannot draw {type}. Download the object to open it with another program.", {
            type: preview.media_type ?? tr("this image type"),
          })}
        </Alert>
      ) : (
        <ImageViewer
          key={source.url}
          url={source.url}
          alt={tr("Preview of {name}", { name: baseName(objectKey) })}
          onError={() => setBrokenImage(source.url)}
        />
      );
  } else if (preview.kind === "too_large") {
    body = (
      <Alert color="yellow" icon={<IconAlertCircle size={16} />} title={tr("Too large to preview")} role="status">
        {preview.media_type === PARQUET_TYPE
          ? tr("This Parquet file is {size}. Parquet files above 8 MiB are not previewed; download the object to open it.", {
              size: formatBytes(preview.size),
            })
          : tr("This {type} is {size}. Images above 8 MiB are not previewed; download the object to open it.", {
              type: preview.media_type ?? tr("image"),
              size: formatBytes(preview.size),
            })}
      </Alert>
    );
  } else if (preview.kind === "empty") {
    body = (
      <Alert color="gray" icon={<IconAlertCircle size={16} />} title={tr("Empty object")} role="status">
        {tr("This object has no content.")}
      </Alert>
    );
  } else {
    body = (
      <Alert color="gray" icon={<IconAlertCircle size={16} />} title={tr("Binary data")} role="status">
        {tr(
          "This object holds data that is neither text nor an image ({size}{type}). Download it to open it with another program.",
          {
            size: formatBytes(preview.size),
            type: preview.content_type === null ? "" : tr(", stored as {type}", { type: preview.content_type }),
          },
        )}
      </Alert>
    );
  }

  return (
    <AppModal
      opened
      onClose={onClose}
      size="xl"
      title={
        <Group gap="xs" wrap="nowrap">
          <KindIcon kind={preview?.kind ?? null} />
          <Text fw={600} className="bb-ellipsis" title={s3Uri(bucket, objectKey)}>
            {tr("Preview: {name}", { name: baseName(objectKey) })}
          </Text>
          {versionId === null ? null : (
            <Badge variant="light" size="sm" title={versionId}>
              {tr("Version {id}", { id: versionId })}
            </Badge>
          )}
        </Group>
      }
    >
      <div onKeyDown={onKeyDown}>
        <Stack gap="sm">
          {preview === null ? null : (
            <Group gap="xs" wrap="wrap">
              <Badge variant="outline" size="sm">
                {preview.kind === "json"
                  ? preview.formatted
                    ? tr("JSON (formatted)")
                    : "JSON"
                  : preview.media_type === "application/xml" && preview.formatted
                    ? tr("XML (formatted)")
                    : tr(preview.kind.replace("_", " "))}
              </Badge>
              <Text size="xs" c="dimmed">
                {[
                  formatBytes(preview.size),
                  preview.content_type,
                  preview.last_modified === null ? null : formatDateTime(preview.last_modified),
                ]
                  .filter((part): part is string => part !== null && part !== "")
                  .join(" · ")}
              </Text>
            </Group>
          )}
          <div aria-busy={loading || undefined} style={loading && current !== null ? { opacity: 0.6 } : undefined}>
            {body}
          </div>
          {pageable ? (
            <Group justify="space-between" wrap="wrap" gap="xs">
              <Group gap={4} wrap="nowrap">
                <Tooltip label={tr("First page (Alt+Home)")}>
                  <ActionIcon variant="default" aria-label={tr("First page")} disabled={!paged || preview.page === 0} onClick={() => goTo(0)}>
                    <IconChevronsLeft size={16} />
                  </ActionIcon>
                </Tooltip>
                <Tooltip label={tr("Previous page (Alt+Left)")}>
                  <ActionIcon
                    variant="default"
                    aria-label={tr("Previous page")}
                    disabled={!paged || preview.page === 0}
                    onClick={() => goTo(preview.page - 1)}
                  >
                    <IconChevronLeft size={16} />
                  </ActionIcon>
                </Tooltip>
                <NumberInput
                  key={`${preview.page}:${preview.page_size}`}
                  aria-label={tr("Page number")}
                  size="xs"
                  w={70}
                  min={1}
                  max={preview.page_count}
                  allowDecimal={false}
                  disabled={!paged}
                  defaultValue={preview.page + 1}
                  onChange={(value) => {
                    typedPage.current = typeof value === "number" ? value : "";
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && typedPage.current !== "") {
                      event.preventDefault();
                      goTo(typedPage.current - 1);
                    }
                  }}
                />
                <Button
                  size="compact-xs"
                  variant="default"
                  disabled={!paged}
                  onClick={() => {
                    if (typedPage.current !== "") {
                      goTo(typedPage.current - 1);
                    }
                  }}
                >
                  {tr("Go")}
                </Button>
                <Tooltip label={tr("Next page (Alt+Right)")}>
                  <ActionIcon
                    variant="default"
                    aria-label={tr("Next page")}
                    disabled={!paged || preview.page >= preview.page_count - 1}
                    onClick={() => goTo(preview.page + 1)}
                  >
                    <IconChevronRight size={16} />
                  </ActionIcon>
                </Tooltip>
                <Tooltip label={tr("Last page (Alt+End)")}>
                  <ActionIcon
                    variant="default"
                    aria-label={tr("Last page")}
                    disabled={!paged || preview.page >= preview.page_count - 1}
                    onClick={() => goTo(preview.page_count - 1)}
                  >
                    <IconChevronsRight size={16} />
                  </ActionIcon>
                </Tooltip>
                <Text size="xs" c="dimmed" aria-live="polite">
                  {tr("Page {cur} of {count}", { cur: formatCount(preview.page + 1), count: formatCount(preview.page_count) })}
                </Text>
              </Group>
              {preview.kind === "table" ? (
                <Text size="xs" c="dimmed">
                  {preview.length === 0
                    ? tr("No rows")
                    : tr("Rows {from} to {to} of {total}", {
                        from: formatCount(preview.offset + 1),
                        to: formatCount(preview.offset + preview.length),
                        total: formatCount(preview.table?.total_rows ?? 0),
                      })}
                </Text>
              ) : null}
              <Group gap="xs" wrap="nowrap" style={preview.kind === "table" ? { display: "none" } : undefined}>
                <Text size="xs" c="dimmed">
                  {tr("Bytes {from} to {to}", {
                    from: formatCount(preview.offset),
                    to: formatCount(Math.max(preview.offset, preview.offset + preview.length - 1)),
                  })}
                </Text>
                <Select
                  aria-label={tr("Page size")}
                  size="xs"
                  w={100}
                  allowDeselect={false}
                  data={PAGE_SIZES}
                  value={sizeChoice}
                  onChange={(value) => setSizeChoice(value ?? String(DEFAULT_PAGE_SIZE))}
                />
                <Button size="compact-xs" variant="default" onClick={applySize}>
                  {tr("Apply")}
                </Button>
              </Group>
            </Group>
          ) : null}
          <Group justify="flex-end">
            <Button variant="default" size="xs" onClick={onClose}>
              {tr("Close")}
            </Button>
          </Group>
        </Stack>
      </div>
    </AppModal>
  );
}
