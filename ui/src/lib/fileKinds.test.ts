import { describe, expect, it } from "vitest";
import { fileKind } from "./fileKinds";

describe("fileKind", () => {
  it("recognises common extensions, ignoring case", () => {
    expect(fileKind("beach.JPG")).toBe("image");
    expect(fileKind("clip.mov")).toBe("video");
    expect(fileKind("song.flac")).toBe("audio");
    expect(fileKind("backup.tar.gz")).toBe("archive");
    expect(fileKind("report.pdf")).toBe("pdf");
    expect(fileKind("config.json")).toBe("code");
    expect(fileKind("events.parquet")).toBe("data");
    expect(fileKind("latest.log")).toBe("text");
  });

  it("falls back to other without a known extension", () => {
    expect(fileKind("snapshot.bin")).toBe("other");
    expect(fileKind("README")).toBe("other");
    expect(fileKind(".env")).toBe("other");
    expect(fileKind("trailing.")).toBe("other");
  });
});
