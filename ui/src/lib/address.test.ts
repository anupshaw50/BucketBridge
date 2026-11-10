import { describe, expect, it } from "vitest";
import { tr } from "../i18n";
import { isAccessPointArn } from "./accessPoint";
import { addressText, parseAddress } from "./address";

describe("parseAddress", () => {
  it("reads s3:// paths and plain bucket/prefix paths", () => {
    expect(parseAddress("s3://demo-photos/photos/2024/")).toEqual({ ok: true, bucket: "demo-photos", prefix: "photos/2024/" });
    expect(parseAddress("  S3://demo-photos/photos  ")).toEqual({ ok: true, bucket: "demo-photos", prefix: "photos/" });
    expect(parseAddress("demo-photos/photos/2024")).toEqual({ ok: true, bucket: "demo-photos", prefix: "photos/2024/" });
    expect(parseAddress("/demo-photos/")).toEqual({ ok: true, bucket: "demo-photos", prefix: "" });
    expect(parseAddress("s3://demo-photos")).toEqual({ ok: true, bucket: "demo-photos", prefix: "" });
    expect(parseAddress("demo-photos")).toEqual({ ok: true, bucket: "demo-photos", prefix: "" });
  });

  it("keeps empty key segments and spaces of the prefix", () => {
    expect(parseAddress("s3://b.ucket/odd//space folder/")).toEqual({ ok: true, bucket: "b.ucket", prefix: "odd//space folder/" });
  });

  it("explains an invalid path", () => {
    expect(parseAddress("   ")).toEqual({ ok: false, message: tr("Enter a path such as s3://bucket/folder/.") });
    expect(parseAddress("s3://")).toEqual({
      ok: false,
      message: tr("The path has no bucket name. Use s3://bucket/folder/."),
    });
    expect(parseAddress("https://example.com/a")).toEqual({ ok: false, message: tr("Only s3:// paths can be opened here.") });
    expect(parseAddress("s3://bad bucket/x")).toEqual({
      ok: false,
      message: tr("\"{bucket}\" is not a valid bucket name.", { bucket: "bad bucket" }),
    });
    expect(parseAddress(`s3://b/${"x".repeat(1025)}`)).toEqual({
      ok: false,
      message: tr("The folder path is longer than 1,024 bytes."),
    });
  });

  it("round-trips the text the address bar shows", () => {
    expect(addressText("demo", "a/b/")).toBe("s3://demo/a/b/");
    expect(addressText("demo", "")).toBe("s3://demo/");
    expect(parseAddress(addressText("demo", "a/b/"))).toEqual({ ok: true, bucket: "demo", prefix: "a/b/" });
  });
});

describe("access point ARNs", () => {
  const AP = "arn:aws:s3:us-west-2:123456789012:accesspoint/my-ap";
  const MRAP = "arn:aws:s3::123456789012:accesspoint/mfzwi23gnjvgw.mrap";

  it("opens an access point or a Multi-Region Access Point like a bucket", () => {
    expect(parseAddress(`s3://${AP}/photos/2024/`)).toEqual({ ok: true, bucket: AP, prefix: "photos/2024/" });
    expect(parseAddress(`s3://${AP}`)).toEqual({ ok: true, bucket: AP, prefix: "" });
    expect(parseAddress(`s3://${MRAP}/docs`)).toEqual({ ok: true, bucket: MRAP, prefix: "docs/" });
    expect(addressText(AP, "a/")).toBe(`s3://${AP}/a/`);
  });

  it("recognises ARNs only in their documented shape", () => {
    expect(isAccessPointArn(AP)).toBe(true);
    expect(isAccessPointArn(MRAP)).toBe(true);
    expect(isAccessPointArn("arn:aws:s3::123456789012:accesspoint/not-multi-region")).toBe(false);
    expect(isAccessPointArn("arn:aws:s3:us-west-2:12345:accesspoint/ap")).toBe(false);
    expect(parseAddress("s3://arn:aws:s3:::plain-bucket/k").ok).toBe(false);
  });
});
