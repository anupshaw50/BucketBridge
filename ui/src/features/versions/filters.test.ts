import { afterEach, describe, expect, it, vi } from "vitest";
import { tr } from "../../i18n";
import { DEFAULT_FILTER, DEFAULT_FORM, toVersionFilter, versionFileName, versionId } from "./filters";
import { imageSource } from "./imageUrl";

describe("version filters", () => {
  it("turns the default form into the default filter", () => {
    expect(toVersionFilter(DEFAULT_FORM)).toEqual({ filter: DEFAULT_FILTER, error: null });
  });

  it("builds every field: kinds, counts and an inclusive UTC date range", () => {
    const result = toVersionFilter({
      showVersions: false,
      deleteMarkers: "only",
      multipleVersions: true,
      moreThan: 2,
      fewerThan: 9,
      from: "2026-10-10",
      to: "2026-10-14",
    });
    expect(result.error).toBeNull();
    expect(result.filter).toEqual({
      show_versions: false,
      delete_markers: "only",
      multiple_versions: true,
      count_greater_than: 2,
      count_less_than: 9,
      // The last day is included: the range ends at the start of the next one.
      modified_after: "2026-10-10T00:00:00Z",
      modified_before: "2026-10-15T00:00:00Z",
    });
    const single = toVersionFilter({ ...DEFAULT_FORM, from: "2026-12-31", to: "2026-12-31" });
    expect(single.filter?.modified_after).toBe("2026-12-31T00:00:00Z");
    expect(single.filter?.modified_before).toBe("2027-01-01T00:00:00Z");
  });

  it("refuses what no key can satisfy, with the rules of the backend", () => {
    const error = (patch: Partial<typeof DEFAULT_FORM>) => toVersionFilter({ ...DEFAULT_FORM, ...patch }).error;
    expect(error({ showVersions: false, deleteMarkers: "exclude" })).toBe(tr("Nothing would be listed: show versions or delete markers."));
    expect(error({ from: "2026-10-14", to: "2026-10-10" })).toBe(tr("The end date is before the start date."));
    expect(error({ from: "14/10/2026" })).toBe(tr("Enter dates as YYYY-MM-DD."));
    expect(error({ to: "2026-02-30" })).toBe(tr("Enter dates as YYYY-MM-DD."));
    expect(error({ moreThan: 5, fewerThan: 6 })).toBe(
      tr("No key can have at least {least} and fewer than {fewer} versions.", { least: 6, fewer: 6 }),
    );
    expect(error({ multipleVersions: true, fewerThan: 2 })).toBe(
      tr("No key can have at least {least} and fewer than {fewer} versions.", { least: 2, fewer: 2 }),
    );
    expect(error({ fewerThan: 1 })).toBe(
      tr("No key can have at least {least} and fewer than {fewer} versions.", { least: 1, fewer: 1 }),
    );
    expect(error({ moreThan: -1 })).toBe(tr("{label} must be a whole number, 0 or more.", { label: tr("More than") }));
    expect(error({ moreThan: 1.5 })).toBe(tr("{label} must be a whole number, 0 or more.", { label: tr("More than") }));
    expect(error({ moreThan: 1, fewerThan: 3 })).toBeNull();
    expect(error({ from: "2026-10-14", to: "2026-10-14" })).toBeNull();
  });

  it("names a downloaded version after its key and its time, without characters files cannot have", () => {
    const name = versionFileName("dir/report.final.pdf", "2026-10-14T09:30:00Z");
    expect(name).toMatch(/^report\.final \(\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}\)\.pdf$/);
    expect(versionFileName("dir/Makefile", "2026-10-14T09:30:00Z")).toMatch(/^Makefile \(\d{4}-/);
    expect(versionFileName("a/.hidden", "2026-10-14T09:30:00Z")).toMatch(/^\.hidden \(/);
    expect(versionFileName("a/b.txt", null)).toBe("b (version).txt");
    expect(versionId("a", "b")).not.toBe(versionId("a\u0000b", ""));
  });
});

describe("image sources", () => {
  const png = "iVBORw0KGgo=";
  const original = URL.createObjectURL.bind(URL);

  afterEach(() => {
    Object.defineProperty(URL, "createObjectURL", { value: original, configurable: true, writable: true });
    vi.restoreAllMocks();
  });

  it("falls back to a data URL without blob URLs, and only for image types", () => {
    Object.defineProperty(URL, "createObjectURL", { value: undefined, configurable: true, writable: true });
    const source = imageSource(png, "image/webp");
    expect(source?.url).toBe(`data:image/webp;base64,${png}`);
    expect(imageSource(png, "text/html")).toBeNull();
    expect(imageSource(png, "image/png;charset=x")).toBeNull();
    expect(imageSource(png, "")).toBeNull();
  });

  it("makes a blob URL of the right type when it can, and releases it", () => {
    const create = vi.spyOn(URL, "createObjectURL").mockReturnValue("blob:mock-1");
    const revoke = vi.spyOn(URL, "revokeObjectURL").mockImplementation(() => undefined);
    const source = imageSource(png, "image/png");
    expect(source?.url).toBe("blob:mock-1");
    const blob = create.mock.calls[0]?.[0];
    expect(blob).toBeInstanceOf(Blob);
    expect((blob as Blob).type).toBe("image/png");
    expect((blob as Blob).size).toBe(8);
    source?.revoke();
    expect(revoke).toHaveBeenCalledWith("blob:mock-1");
  });
});
