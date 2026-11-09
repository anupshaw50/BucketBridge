import { describe, expect, it } from "vitest";
import { clampPan, clampZoom, fitZoom, MAX_ZOOM, MIN_ZOOM, panBy, percent, wheelFactor, zoomAt } from "./imageView";

describe("image view arithmetic", () => {
  it("fits an image into its frame without enlarging it", () => {
    expect(fitZoom({ width: 2000, height: 1000 }, { width: 1000, height: 1000 })).toBe(0.5);
    expect(fitZoom({ width: 100, height: 50 }, { width: 1000, height: 1000 })).toBe(1);
    expect(fitZoom({ width: 1000, height: 4000 }, { width: 1000, height: 1000 })).toBe(0.25);
    // Sizes that are not known (a frame that is not laid out yet).
    expect(fitZoom({ width: 0, height: 0 }, { width: 100, height: 100 })).toBe(1);
    expect(fitZoom({ width: 10, height: 10 }, { width: 0, height: 0 })).toBe(1);
  });

  it("keeps the zoom between its limits", () => {
    expect(clampZoom(1000)).toBe(MAX_ZOOM);
    expect(clampZoom(0)).toBe(MIN_ZOOM);
    expect(clampZoom(Number.NaN)).toBe(1);
    expect(percent(1.256)).toBe(126);
  });

  it("zooms around a point so that the point stays where it is", () => {
    const view = { zoom: 1, x: 0, y: 0 };
    const zoomed = zoomAt(view, 2, { x: 100, y: 50 });
    expect(zoomed.zoom).toBe(2);
    // The image point under (100, 50) was (100, 50); at zoom 2 it is still under the pointer.
    expect(zoomed.x + 100 * 2).toBeCloseTo(100, 5);
    expect(zoomed).toEqual({ zoom: 2, x: -100, y: -50 });
    // Zooming at the centre does not move the image.
    expect(zoomAt({ zoom: 1, x: 7, y: 3 }, 2, { x: 0, y: 0 })).toEqual({ zoom: 2, x: 14, y: 6 });
    // At the limit nothing changes.
    expect(zoomAt({ zoom: MAX_ZOOM, x: 5, y: 5 }, 2, { x: 10, y: 10 })).toEqual({ zoom: MAX_ZOOM, x: 5, y: 5 });
  });

  it("pans and never lets the image leave the frame", () => {
    expect(panBy({ zoom: 1, x: 1, y: 2 }, 3, 4)).toEqual({ zoom: 1, x: 4, y: 6 });
    const image = { width: 1000, height: 500 };
    const frame = { width: 400, height: 300 };
    // Drawn at 100 %: at most (1000 + 400) / 2 - 100 = 600 to the side.
    expect(clampPan({ zoom: 1, x: 9999, y: -9999 }, image, frame)).toEqual({ zoom: 1, x: 600, y: -(250 + 150 - 75) });
    // An image far smaller than the frame stays about in the middle.
    expect(clampPan({ zoom: 0.1, x: 500, y: 0 }, image, frame).x).toBeLessThan(200);
    expect(clampPan({ zoom: 1, x: 50, y: 50 }, { width: 0, height: 0 }, frame)).toEqual({ zoom: 1, x: 50, y: 50 });
  });

  it("turns a wheel into a zoom factor, finer for a pinch", () => {
    expect(wheelFactor(-100, false, 0)).toBeGreaterThan(1);
    expect(wheelFactor(100, false, 0)).toBeLessThan(1);
    expect(wheelFactor(0, false, 0)).toBe(1);
    expect(wheelFactor(-10, true, 0)).toBeGreaterThan(wheelFactor(-10, false, 0));
    // Lines and pages are larger steps, and the step is bounded.
    expect(wheelFactor(-3, false, 1)).toBeGreaterThan(wheelFactor(-3, false, 0));
    expect(wheelFactor(-100000, false, 0)).toBe(wheelFactor(-120, false, 0));
  });
});
