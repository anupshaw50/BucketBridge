import { ActionIcon, Group, Text, Tooltip } from "@mantine/core";
import { IconArrowsMaximize, IconZoomIn, IconZoomOut, IconZoomReset } from "@tabler/icons-react";
import { type KeyboardEvent, type PointerEvent, useCallback, useEffect, useRef, useState } from "react";
import { tr } from "../../i18n";
import { clampPan, fitZoom, type Point, panBy, percent, type Size, type View, wheelFactor, ZOOM_STEP, zoomAt } from "./imageView";

export interface ImageViewerProps {
  url: string;
  alt: string;
  onError: () => void;
}

/** Pixels one arrow key press moves the picture. */
const KEY_PAN = 40;

/**
 * An image that can be zoomed with the wheel, a trackpad pinch, two fingers or the buttons, and
 * moved by dragging or with the arrow keys. The picture is a plain `<img>`, so an animated GIF
 * plays. `null` as the view means "fit to the frame".
 */
export function ImageViewer({ url, alt, onError }: ImageViewerProps) {
  const frame = useRef<HTMLDivElement>(null);
  const image = useRef<HTMLImageElement>(null);
  const [view, setView] = useState<View | null>(null);
  const viewRef = useRef<View | null>(null);
  useEffect(() => {
    viewRef.current = view;
  }, [view]);
  const pointers = useRef(new Map<number, Point>());
  const gesture = useRef<{ distance: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  const sizes = useCallback((): { image: Size; frame: Size } => {
    const box = frame.current?.getBoundingClientRect();
    return {
      image: { width: image.current?.naturalWidth ?? 0, height: image.current?.naturalHeight ?? 0 },
      frame: { width: box?.width ?? 0, height: box?.height ?? 0 },
    };
  }, []);

  /** The view in effect, which for "fit" is the zoom that fits. */
  const current = useCallback((): View => {
    const now = viewRef.current;
    if (now !== null) {
      return now;
    }
    const { image: natural, frame: box } = sizes();
    return { zoom: fitZoom(natural, box), x: 0, y: 0 };
  }, [sizes]);

  const apply = useCallback(
    (next: View) => {
      const { image: natural, frame: box } = sizes();
      setView(clampPan(next, natural, box));
    },
    [sizes],
  );

  const zoomBy = useCallback(
    (factor: number, at: Point = { x: 0, y: 0 }) => {
      apply(zoomAt(current(), factor, at));
    },
    [apply, current],
  );

  // A wheel listener that is not passive, so that the page does not scroll while zooming.
  useEffect(() => {
    const element = frame.current;
    if (element === null) {
      return undefined;
    }
    const onWheel = (event: WheelEvent) => {
      event.preventDefault();
      const box = element.getBoundingClientRect();
      const at = { x: event.clientX - box.left - box.width / 2, y: event.clientY - box.top - box.height / 2 };
      zoomBy(wheelFactor(event.deltaY, event.ctrlKey, event.deltaMode), Number.isFinite(at.x) && Number.isFinite(at.y) ? at : { x: 0, y: 0 });
    };
    element.addEventListener("wheel", onWheel, { passive: false });
    return () => element.removeEventListener("wheel", onWheel);
  }, [zoomBy]);

  const distance = () => {
    const [a, b] = [...pointers.current.values()];
    return a === undefined || b === undefined ? 0 : Math.hypot(a.x - b.x, a.y - b.y);
  };

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    pointers.current.set(event.pointerId, { x: event.clientX, y: event.clientY });
    // Not every web view (nor jsdom) has pointer capture.
    const target: { setPointerCapture?: (id: number) => void } = event.currentTarget;
    target.setPointerCapture?.(event.pointerId);
    gesture.current = pointers.current.size === 2 ? { distance: distance() } : null;
    setDragging(true);
  };
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const before = pointers.current.get(event.pointerId);
    if (before === undefined) {
      return;
    }
    const now = { x: event.clientX, y: event.clientY };
    pointers.current.set(event.pointerId, now);
    if (pointers.current.size === 2 && gesture.current !== null) {
      const next = distance();
      if (gesture.current.distance > 0 && next > 0) {
        const box = frame.current?.getBoundingClientRect();
        const [a, b] = [...pointers.current.values()];
        const middle =
          box === undefined || a === undefined || b === undefined
            ? { x: 0, y: 0 }
            : { x: (a.x + b.x) / 2 - box.left - box.width / 2, y: (a.y + b.y) / 2 - box.top - box.height / 2 };
        zoomBy(next / gesture.current.distance, middle);
      }
      gesture.current = { distance: next };
    } else if (pointers.current.size === 1) {
      apply(panBy(current(), now.x - before.x, now.y - before.y));
    }
  };
  const onPointerEnd = (event: PointerEvent<HTMLDivElement>) => {
    pointers.current.delete(event.pointerId);
    gesture.current = null;
    if (pointers.current.size === 0) {
      setDragging(false);
    }
  };

  const fit = () => setView(null);
  const actual = () => apply({ zoom: 1, x: 0, y: 0 });
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.altKey || event.ctrlKey || event.metaKey) {
      return;
    }
    const pan = (dx: number, dy: number) => apply(panBy(current(), dx, dy));
    switch (event.key) {
      case "+":
      case "=":
        zoomBy(ZOOM_STEP);
        break;
      case "-":
        zoomBy(1 / ZOOM_STEP);
        break;
      case "0":
        fit();
        break;
      case "1":
        actual();
        break;
      case "ArrowLeft":
        pan(KEY_PAN, 0);
        break;
      case "ArrowRight":
        pan(-KEY_PAN, 0);
        break;
      case "ArrowUp":
        pan(0, KEY_PAN);
        break;
      case "ArrowDown":
        pan(0, -KEY_PAN);
        break;
      default:
        return;
    }
    event.preventDefault();
  };

  const label = view === null ? tr("Fit") : `${percent(view.zoom)}%`;
  const style =
    view === null
      ? { maxWidth: "100%", maxHeight: "100%", transform: "translate(-50%, -50%)" }
      : {
          maxWidth: "none",
          maxHeight: "none",
          transform: `translate(-50%, -50%) translate(${view.x}px, ${view.y}px) scale(${view.zoom})`,
        };

  return (
    <div className="bb-preview-viewer">
      <Group gap={4} wrap="nowrap" className="bb-preview-zoombar">
        <Tooltip label={tr("Zoom out (-)")}>
          <ActionIcon variant="default" size="sm" aria-label={tr("Zoom out")} onClick={() => zoomBy(1 / ZOOM_STEP)}>
            <IconZoomOut size={16} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={tr("Zoom in (+)")}>
          <ActionIcon variant="default" size="sm" aria-label={tr("Zoom in")} onClick={() => zoomBy(ZOOM_STEP)}>
            <IconZoomIn size={16} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={tr("Fit to window (0)")}>
          <ActionIcon variant={view === null ? "filled" : "default"} size="sm" aria-label={tr("Fit to window")} onClick={fit}>
            <IconArrowsMaximize size={16} />
          </ActionIcon>
        </Tooltip>
        <Tooltip label={tr("Actual size (1)")}>
          <ActionIcon variant="default" size="sm" aria-label={tr("Actual size")} onClick={actual}>
            <IconZoomReset size={16} />
          </ActionIcon>
        </Tooltip>
        <Text size="xs" c="dimmed" aria-live="polite" aria-label={tr("Zoom level")}>
          {label}
        </Text>
      </Group>
      <div
        ref={frame}
        className="bb-preview-frame bb-preview-pan"
        data-dragging={dragging || undefined}
        tabIndex={0}
        role="group"
        aria-label={tr("Image viewer. Plus and minus zoom, arrow keys move, 0 fits the window, 1 shows the actual size.")}
        onKeyDown={onKeyDown}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerEnd}
        onPointerCancel={onPointerEnd}
        onDoubleClick={() => (view === null ? actual() : fit())}
      >
        <img
          ref={image}
          className="bb-preview-image"
          src={url}
          alt={alt}
          draggable={false}
          style={style}
          onLoad={() => {
            // The frame and the picture are measured now; keep a zoomed view inside the frame.
            if (viewRef.current !== null) {
              apply(viewRef.current);
            }
          }}
          onError={onError}
        />
      </div>
    </div>
  );
}
