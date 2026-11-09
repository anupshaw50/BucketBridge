/** Zoom and pan arithmetic of the image preview. Pure, so it can be tested without a browser. */

export const MIN_ZOOM = 0.05;
export const MAX_ZOOM = 32;
/** What one click of a zoom button or one key press multiplies the zoom by. */
export const ZOOM_STEP = 1.25;

export interface Size {
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

/** The image is drawn at `zoom` times its own size, moved by (`x`, `y`) pixels from the centre of the frame. */
export interface View extends Point {
  zoom: number;
}

export function clampZoom(zoom: number): number {
  if (!Number.isFinite(zoom)) {
    return 1;
  }
  return Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, zoom));
}

/** The zoom at which the whole image is inside the frame (never above 100 %). 1 when a size is not known yet. */
export function fitZoom(image: Size, frame: Size): number {
  if (image.width <= 0 || image.height <= 0 || frame.width <= 0 || frame.height <= 0) {
    return 1;
  }
  return clampZoom(Math.min(1, frame.width / image.width, frame.height / image.height));
}

/**
 * Zooms by `factor` keeping the point `at` (relative to the centre of the frame) over the same
 * spot of the image, as wheel and pinch zoom feel natural only that way.
 */
export function zoomAt(view: View, factor: number, at: Point): View {
  const zoom = clampZoom(view.zoom * factor);
  const ratio = zoom / view.zoom;
  return { zoom, x: at.x - (at.x - view.x) * ratio, y: at.y - (at.y - view.y) * ratio };
}

/** Moves the image by (`dx`, `dy`) pixels. */
export function panBy(view: View, dx: number, dy: number): View {
  return { ...view, x: view.x + dx, y: view.y + dy };
}

/**
 * Keeps at least a quarter of the frame covered by the image in each direction, so that a
 * drag can never push the picture out of reach. A frame or image of unknown size leaves the view alone.
 */
export function clampPan(view: View, image: Size, frame: Size): View {
  if (image.width <= 0 || image.height <= 0 || frame.width <= 0 || frame.height <= 0) {
    return view;
  }
  const limit = (drawn: number, shown: number) => (drawn + shown) / 2 - shown / 4;
  const maxX = Math.max(0, limit(image.width * view.zoom, frame.width));
  const maxY = Math.max(0, limit(image.height * view.zoom, frame.height));
  return { ...view, x: Math.min(maxX, Math.max(-maxX, view.x)), y: Math.min(maxY, Math.max(-maxY, view.y)) };
}

/** The factor a wheel turn of `deltaY` zooms by; a pinch gesture on a trackpad arrives as a wheel with Ctrl held and is finer. */
export function wheelFactor(deltaY: number, ctrl: boolean, deltaMode: number): number {
  const lines = deltaMode === 1 ? deltaY * 16 : deltaMode === 2 ? deltaY * 400 : deltaY;
  const bounded = Math.max(-120, Math.min(120, lines));
  return Math.exp(-bounded * (ctrl ? 0.01 : 0.0015));
}

export function percent(zoom: number): number {
  return Math.round(zoom * 100);
}
