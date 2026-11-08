import { describe, expect, it } from "vitest";
import { tr } from "../i18n";
import { cloneSettings, DEFAULT_SETTINGS, pageOfField, validateSettings } from "./settings";
import {
  parseBypassList,
  validateBucketName,
  validateBypassEntry,
  validateEndpoint,
  validateHost,
  validateRegion,
} from "./validation";

describe("bucket names", () => {
  it("accepts valid names", () => {
    for (const name of ["abc", "my-bucket", "logs.example.com", "a1b2c3", "x".repeat(63)]) {
      expect(validateBucketName(name), name).toBeNull();
    }
  });

  it("rejects names that break the S3 rules", () => {
    expect(validateBucketName("")).toBe(tr("Enter a bucket name."));
    expect(validateBucketName("ab")).toBe(tr("Bucket names must be between 3 and 63 characters long."));
    expect(validateBucketName("x".repeat(64))).toBe(tr("Bucket names must be between 3 and 63 characters long."));
    expect(validateBucketName("My-Bucket")).toBe(tr("Use only lowercase letters, digits, dots (.) and hyphens (-)."));
    expect(validateBucketName("under_score")).toBe(tr("Use only lowercase letters, digits, dots (.) and hyphens (-)."));
    expect(validateBucketName("-start")).toBe(tr("Bucket names must begin and end with a letter or digit."));
    expect(validateBucketName("end.")).toBe(tr("Bucket names must begin and end with a letter or digit."));
    expect(validateBucketName("a..b")).toBe(tr("Bucket names cannot contain two adjacent dots."));
    expect(validateBucketName("a.-b")).toBe(tr("A dot cannot be next to a hyphen."));
    expect(validateBucketName("192.168.5.4")).toBe(tr("Bucket names cannot look like an IP address."));
    expect(validateBucketName("xn--bucket")).toBe(tr("Bucket names cannot start with \"{prefix}\".", { prefix: "xn--" }));
    expect(validateBucketName("bucket-s3alias")).toBe(tr("Bucket names cannot end with \"{suffix}\".", { suffix: "-s3alias" }));
  });
});

describe("connection fields", () => {
  it("validates regions and endpoints", () => {
    expect(validateRegion("us-east-1")).toBeNull();
    expect(validateRegion("auto")).toBeNull();
    expect(validateRegion("US-EAST-1")).not.toBeNull();
    expect(validateRegion("")).not.toBeNull();
    expect(validateEndpoint("")).toBeNull();
    expect(validateEndpoint("https://storage.example.com")).toBeNull();
    expect(validateEndpoint("http://localhost:9000")).toBeNull();
    expect(validateEndpoint("storage.example.com")).toBe(tr("Enter a full URL such as https://storage.example.com."));
    expect(validateEndpoint("ftp://storage.example.com")).toBe(tr("The endpoint must start with https:// or http://."));
    expect(validateEndpoint("https://user:pw@storage.example.com")).toBe(tr("Do not put credentials in the endpoint URL."));
    expect(validateEndpoint("https://storage.example.com/?a=1")).toBe(tr("The endpoint cannot contain a query string or fragment."));
  });

  it("validates proxy hosts and bypass entries", () => {
    expect(validateHost("proxy.example.com")).toBeNull();
    expect(validateHost("10.0.0.1")).toBeNull();
    expect(validateHost("[::1]")).toBeNull();
    expect(validateHost("http://proxy")).not.toBeNull();
    expect(validateHost("proxy:8080")).toBe(tr("Enter the port in the port field."));
    expect(validateBypassEntry("localhost")).toBeNull();
    expect(validateBypassEntry("*.example.com")).toBeNull();
    expect(validateBypassEntry("10.0.0.0/8")).toBeNull();
    expect(validateBypassEntry("fd00::/8")).toBeNull();
    expect(validateBypassEntry("192.168.1.10")).toBeNull();
    expect(validateBypassEntry("fd00::1")).toBeNull();
    expect(validateBypassEntry("[::1]")).toBeNull();
    expect(validateBypassEntry(":::")).not.toBeNull();
    expect(validateBypassEntry("1:2:3:4:5:6:7:8:9")).not.toBeNull();
    expect(validateBypassEntry("10.0.0.0/33")).toBe(tr("\"{entry}\" has a prefix length above 32.", { entry: "10.0.0.0/33" }));
    expect(validateBypassEntry("300.1.1.1/8")).toBe(tr("\"{entry}\" is not a valid CIDR range.", { entry: "300.1.1.1/8" }));
    expect(validateBypassEntry("bad host")).toBe(tr("\"{entry}\" is not a host name, \"*.suffix\" pattern, IP address or CIDR range.", { entry: "bad host" }));
    expect(parseBypassList(" a.example.com \n\n*.b.example.com, 10.0.0.0/8 ")).toEqual([
      "a.example.com",
      "*.b.example.com",
      "10.0.0.0/8",
    ]);
  });
});

describe("settings validation", () => {
  it("accepts the documented defaults", () => {
    expect(validateSettings(DEFAULT_SETTINGS)).toEqual({});
  });

  it("enforces the documented ranges and integers", () => {
    const settings = cloneSettings(DEFAULT_SETTINGS);
    settings.advanced.list_page_size = 1001;
    settings.advanced.max_attempts = 0;
    settings.transfer.part_size_mib = 4;
    settings.transfer.max_concurrent_tasks = 2.5;
    settings.proxy.port = Number.NaN;
    settings.transfer.bandwidth_limit_kibps = 0;
    const errors = validateSettings(settings);
    expect(Object.keys(errors).sort()).toEqual([
      "advanced.list_page_size",
      "advanced.max_attempts",
      "proxy.port",
      "transfer.bandwidth_limit_kibps",
      "transfer.max_concurrent_tasks",
      "transfer.part_size_mib",
    ]);
    expect(errors["advanced.list_page_size"]).toBe(tr("Enter a whole number from {min} to {max}.", { min: "1", max: "1,000" }));
    expect(pageOfField("transfer.part_size_mib")).toBe("transfer");
  });

  it("uses the ranges of core's Settings::validate", () => {
    const settings = cloneSettings(DEFAULT_SETTINGS);
    settings.transfer.bandwidth_limit_kibps = 104_857_600;
    settings.transfer.retry_attempts = 20;
    expect(validateSettings(settings)).toEqual({});
    settings.transfer.bandwidth_limit_kibps = 104_857_601;
    settings.transfer.retry_attempts = 21;
    expect(Object.keys(validateSettings(settings)).sort()).toEqual([
      "transfer.bandwidth_limit_kibps",
      "transfer.retry_attempts",
    ]);
  });

  it("allows an unlimited bandwidth and requires a host for an enabled proxy", () => {
    const settings = cloneSettings(DEFAULT_SETTINGS);
    settings.transfer.bandwidth_limit_kibps = null;
    settings.proxy.enabled = true;
    expect(Object.keys(validateSettings(settings))).toEqual(["proxy.host"]);
    settings.proxy.host = "proxy.example.com";
    settings.proxy.bypass = ["*.example.com", "not valid"];
    expect(Object.keys(validateSettings(settings))).toEqual(["proxy.bypass"]);
    settings.general.default_download_dir = "";
    expect(validateSettings(settings)["general.default_download_dir"]).toBe(tr("Choose a folder or select ask every time."));
  });
});
