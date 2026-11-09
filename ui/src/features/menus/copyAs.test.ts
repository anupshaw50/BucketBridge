import { afterEach, describe, expect, it, vi } from "vitest";
import { styledUrl, supportsVirtualHosted } from "../../lib/bucketUrl";
import { folderEntry } from "../../lib/objects";
import { MockBackend } from "../../mock/mockBackend";
import { AppModel } from "../../state/model";
import { immediateScheduler } from "../../state/scheduler";
import { TestPlatform } from "../../test/renderApp";
import { bucketContext, itemsContext, type MenuContext } from "./context";
import type { ActionId } from "./ids";
import { ACTIONS } from "./registry";
import { resolveMenu } from "./resolve";

const live: { stop: () => void; mock: MockBackend }[] = [];

afterEach(() => {
  while (live.length > 0) {
    const item = live.pop();
    item?.stop();
    item?.mock.dispose();
  }
});

async function loadedModel() {
  const mock = new MockBackend({ autoRun: false, latencyMs: 0, logObjects: 5, manyFiles: 0 });
  const platform = new TestPlatform();
  const model = new AppModel({ backend: mock, platform, scheduler: immediateScheduler });
  live.push({ stop: model.start(), mock });
  await vi.waitFor(() => {
    expect(model.app.getState().ready).toBe(true);
    expect(model.browser.state.bucketList.status).toBe("ready");
  });
  return { model, platform, mock };
}

const AWS = { endpoint: null, path_style: false };
const CUSTOM_PATH = { endpoint: "https://storage.example.com/", path_style: true };
const CUSTOM_VIRTUAL = { endpoint: "http://minio.local:9000", path_style: false };

describe("styledUrl", () => {
  it("builds virtual-hosted and path-style URLs for AWS in the bucket's region", () => {
    expect(styledUrl(AWS, "photos", "eu-west-1", "a/b c.txt", "virtual")).toBe(
      "https://photos.s3.eu-west-1.amazonaws.com/a/b%20c.txt",
    );
    expect(styledUrl(AWS, "photos", "eu-west-1", "a/b c.txt", "path")).toBe(
      "https://s3.eu-west-1.amazonaws.com/photos/a/b%20c.txt",
    );
    expect(styledUrl(AWS, "photos", null, "", "virtual")).toBe("https://photos.s3.us-east-1.amazonaws.com/");
    expect(styledUrl(AWS, "photos", null, "", "path")).toBe("https://s3.us-east-1.amazonaws.com/photos");
  });

  it("encodes special characters of a key but keeps its slashes", () => {
    expect(styledUrl(AWS, "b", "us-east-1", "x/it's (1)#?.txt", "path")).toBe(
      "https://s3.us-east-1.amazonaws.com/b/x/it%27s%20%281%29%23%3F.txt",
    );
  });

  it("follows a custom endpoint and its addressing style", () => {
    expect(styledUrl(CUSTOM_PATH, "data", null, "k.txt", "path")).toBe("https://storage.example.com/data/k.txt");
    expect(styledUrl(CUSTOM_VIRTUAL, "data", null, "k.txt", "virtual")).toBe("http://data.minio.local:9000/k.txt");
    expect(styledUrl(CUSTOM_VIRTUAL, "data", null, "", "virtual")).toBe("http://data.minio.local:9000/");
    expect(supportsVirtualHosted(AWS)).toBe(true);
    expect(supportsVirtualHosted(CUSTOM_VIRTUAL)).toBe(true);
    expect(supportsVirtualHosted(CUSTOM_PATH)).toBe(false);
  });
});

describe("Copy as", () => {
  const COPY_IDS: ActionId[] = ["copy.name", "copy.key", "copy.uri", "copy.url-virtual", "copy.url-path"];

  async function copy(id: ActionId, ctx: MenuContext, platform: TestPlatform) {
    await ACTIONS[id]?.run(ctx);
    return platform.copied.at(-1);
  }

  it("copies one line per selected object, folder or bucket", async () => {
    const { model, platform } = await loadedModel();
    await model.browser.openLocation("demo-photos", "");
    await vi.waitFor(() => expect(model.browser.state.listing.status).toBe("ready"));
    const file = model.browser.state.visible.find((entry) => entry.key === "readme.md");
    if (file === undefined) {
      throw new Error("no readme.md");
    }
    const folder = folderEntry("photos/2024/");
    const ctx = itemsContext(model, "demo-photos", [folder, file]);
    expect(await copy("copy.name", ctx, platform)).toBe("2024\nreadme.md");
    expect(await copy("copy.key", ctx, platform)).toBe("photos/2024/\nreadme.md");
    expect(await copy("copy.uri", ctx, platform)).toBe("s3://demo-photos/photos/2024/\ns3://demo-photos/readme.md");
    expect(await copy("copy.url-virtual", ctx, platform)).toBe(
      "https://demo-photos.s3.us-east-1.amazonaws.com/photos/2024/\nhttps://demo-photos.s3.us-east-1.amazonaws.com/readme.md",
    );
    expect(await copy("copy.url-path", ctx, platform)).toBe(
      "https://s3.us-east-1.amazonaws.com/demo-photos/photos/2024/\nhttps://s3.us-east-1.amazonaws.com/demo-photos/readme.md",
    );

    const buckets = model.browser.state.bucketList.buckets.filter((bucket) =>
      ["demo-photos", "demo-backups"].includes(bucket.name),
    );
    const bucketCtx = bucketContext(model, buckets);
    expect(await copy("copy.name", bucketCtx, platform)).toBe("demo-backups\ndemo-photos");
    expect(await copy("copy.uri", bucketCtx, platform)).toBe("s3://demo-backups/\ns3://demo-photos/");
    // A bucket has no key.
    expect(ACTIONS["copy.key"]?.visible?.(bucketCtx)).toBe(false);
    expect(COPY_IDS.every((id) => id in ACTIONS)).toBe(true);
  });

  it("hides the virtual-hosted URL for an account that uses path-style addressing", async () => {
    const { model, mock } = await loadedModel();
    const accounts = (await mock.list_accounts()).accounts;
    const external = accounts.find((account) => account.endpoint !== null);
    expect(external?.path_style).toBe(true);
    const ctx = {
      ...bucketContext(model, [
        { name: "archive-2025", region: null, creation_date: null, owner: null, kind: "external" as const },
      ]),
      account: external ?? null,
    };
    const labels = resolveMenu("bucket", ctx).flatMap((item) => (item.type === "submenu" && item.id === "copy-as" ? item.items : []));
    expect(labels.map((item) => (item.type === "action" ? item.id : "-"))).toEqual(["copy.name", "copy.uri", "copy.url-path"]);
  });
});
