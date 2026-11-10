import { describe, expect, it } from "vitest";
import { MOCK_REGIONS } from "../mock/regionsMock";
import { arnPrefix, partitionOf } from "./regions";

describe("regions", () => {
  it("knows the partition of listed regions", () => {
    expect(partitionOf(MOCK_REGIONS, "eu-west-1")).toBe("aws");
    expect(partitionOf(MOCK_REGIONS, "cn-north-1")).toBe("aws-cn");
    expect(partitionOf(MOCK_REGIONS, "us-gov-east-1")).toBe("aws-us-gov");
  });

  it("classifies unlisted regions by name", () => {
    expect(partitionOf([], "cn-southwest-9")).toBe("aws-cn");
    expect(partitionOf([], "us-gov-central-1")).toBe("aws-us-gov");
    expect(partitionOf([], "eusc-fr-east-1")).toBe("aws-eusc");
    expect(partitionOf([], "auto")).toBe("aws");
  });

  it("builds the ARN prefix", () => {
    expect(arnPrefix(MOCK_REGIONS, "cn-northwest-1")).toBe("arn:aws-cn");
    expect(arnPrefix(MOCK_REGIONS, "us-east-1")).toBe("arn:aws");
  });
});
