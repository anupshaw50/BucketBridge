import { describe, expect, it } from "vitest";
import { startLocationFromSearch } from "./startLocation";

describe("startLocationFromSearch", () => {
  it("reads and decodes the location", () => {
    expect(startLocationFromSearch("?bb_account=a-1&bb_bucket=data&bb_prefix=my%20docs%2F%C3%A9%26x%3D1%2F")).toEqual({
      account_id: "a-1",
      bucket: "data",
      prefix: "my docs/é&x=1/",
    });
  });

  it("defaults the prefix and ignores other keys", () => {
    expect(startLocationFromSearch("?fixture=empty&bb_account=a&bb_bucket=b")).toEqual({ account_id: "a", bucket: "b", prefix: "" });
  });

  it("is null without an account or a bucket", () => {
    expect(startLocationFromSearch("")).toBeNull();
    expect(startLocationFromSearch("?bb_bucket=b")).toBeNull();
    expect(startLocationFromSearch("?bb_account=a")).toBeNull();
    expect(startLocationFromSearch("?bb_account=&bb_bucket=b")).toBeNull();
  });
});
