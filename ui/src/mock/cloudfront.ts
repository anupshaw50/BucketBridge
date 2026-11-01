import { BackendError } from "../api/errors";
import type {
  CloudFrontDistribution,
  CloudFrontInvalidation,
  CloudFrontPage,
  CreatedDistribution,
  DistributionEdit,
  InvalidationPage,
  NewDistribution,
  PathSet,
} from "../api/types";
import { validatePaths } from "../features/cloudfront/paths";

const PAGE = 2;
const HISTORY_PAGE = 3;

function fail(code: "usage" | "not_found" | "conflict", message: string): never {
  throw new BackendError(code, message);
}

function distribution(id: string, bucket: string | null, enabled: boolean, comment: string): CloudFrontDistribution {
  return {
    id,
    arn: `arn:aws:cloudfront::123456789012:distribution/${id}`,
    domain_name: `${id.toLowerCase()}.cloudfront.net`,
    status: "Deployed",
    enabled,
    comment,
    aliases: [],
    price_class: "PriceClass_All",
    last_modified: "2026-10-20T08:00:00.000Z",
    origins: [
      {
        id: bucket === null ? "api" : `s3-${bucket}`,
        domain_name: bucket === null ? "api.example.org" : `${bucket}.s3.us-east-1.amazonaws.com`,
        origin_path: "",
        bucket,
        website: false,
        access_control: bucket !== null,
      },
    ],
    default_root_object: "index.html",
    in_progress_invalidations: 0,
    etag: "E-1",
  };
}

/** Adds a second, non-S3 origin, so the S3 origin can be detached without emptying the distribution. */
function withApiOrigin(entry: CloudFrontDistribution): CloudFrontDistribution {
  entry.origins.push({ id: "api", domain_name: "api.example.org", origin_path: "", bucket: null, website: false, access_control: false });
  return entry;
}

/** The mock CloudFront of the demo account: three distributions, invalidations that complete on their own. */
export class MockCloudFront {
  private readonly distributions: CloudFrontDistribution[] = [
    distribution("EDEMO0001", "demo-website", true, "Demo website"),
    distribution("EDEMO0002", null, false, "Other service"),
    withApiOrigin(distribution("EDEMO0003", "demo-photos", true, "Photos")),
  ];
  private readonly invalidations = new Map<string, CloudFrontInvalidation[]>([
    [
      "EDEMO0001",
      [
        { id: "I0003", status: "Completed", create_time: "2026-10-21T10:00:00.000Z", paths: ["/css/*"] },
        { id: "I0002", status: "Completed", create_time: "2026-10-20T10:00:00.000Z", paths: ["/index.html"] },
        { id: "I0001", status: "Completed", create_time: "2026-10-19T10:00:00.000Z", paths: ["/*"] },
        { id: "I0000", status: "Completed", create_time: "2026-10-18T10:00:00.000Z", paths: ["/old/*"] },
      ],
    ],
  ]);
  private sets: PathSet[] = [];
  private counter = 4;
  private etag = 1;

  private find(id: string): CloudFrontDistribution {
    const found = this.distributions.find((entry) => entry.id === id);
    if (found === undefined) {
      fail("not_found", `Distribution ${id} does not exist (NoSuchDistribution).`);
    }
    return found;
  }

  private touch(entry: CloudFrontDistribution): CloudFrontDistribution {
    this.etag += 1;
    entry.etag = `E-${this.etag}`;
    entry.last_modified = new Date().toISOString();
    return structuredClone(entry);
  }

  private checkEtag(entry: CloudFrontDistribution, expected: string | null): void {
    if (expected !== null && expected !== entry.etag) {
      fail("conflict", `Distribution ${entry.id} was changed since it was loaded; reload it and try again.`);
    }
  }

  list(marker: string | null): CloudFrontPage {
    const start = marker === null ? 0 : Number(marker);
    const items = this.distributions.slice(start, start + PAGE).map((entry) => ({
      ...structuredClone(entry),
      default_root_object: null,
      in_progress_invalidations: null,
      etag: null,
    }));
    const next = start + PAGE;
    return { items, next_marker: next < this.distributions.length ? String(next) : null };
  }

  get(id: string): CloudFrontDistribution {
    return structuredClone(this.find(id));
  }

  forBucket(bucket: string): CloudFrontDistribution[] {
    return this.distributions
      .filter((entry) => entry.origins.some((origin) => origin.bucket === bucket))
      .map((entry) => structuredClone(entry));
  }

  hasDistribution(bucket: string): boolean {
    return this.distributions.some((entry) => entry.origins.some((origin) => origin.bucket === bucket));
  }

  create(input: NewDistribution): CreatedDistribution {
    const created = distribution(`ENEW${String(this.counter).padStart(5, "0")}`, input.bucket, input.enabled, input.comment);
    this.counter += 1;
    created.default_root_object = input.default_root_object;
    this.distributions.push(created);
    return {
      distribution: structuredClone(created),
      origin_access_control_id: "OAC-MOCK",
      bucket_policy: `{"Statement":[{"Resource":"arn:aws:s3:::${input.bucket}/*"}]}`,
    };
  }

  update(id: string, edit: DistributionEdit, expected: string | null): CloudFrontDistribution {
    const entry = this.find(id);
    this.checkEtag(entry, expected);
    if (edit.comment !== undefined && edit.comment !== null) entry.comment = edit.comment;
    if (edit.aliases !== undefined && edit.aliases !== null) entry.aliases = [...edit.aliases];
    if (edit.default_root_object !== undefined && edit.default_root_object !== null) entry.default_root_object = edit.default_root_object;
    if (edit.price_class !== undefined && edit.price_class !== null) entry.price_class = edit.price_class;
    if (edit.enabled !== undefined && edit.enabled !== null) entry.enabled = edit.enabled;
    return this.touch(entry);
  }

  detach(id: string, bucket: string, expected: string | null): CloudFrontDistribution {
    const entry = this.find(id);
    this.checkEtag(entry, expected);
    if (!entry.origins.some((origin) => origin.bucket === bucket)) {
      fail("not_found", `The distribution has no origin in bucket '${bucket}'.`);
    }
    if (entry.origins.every((origin) => origin.bucket === bucket)) {
      fail("conflict", `Bucket '${bucket}' is the only origin of the distribution; delete the distribution instead.`);
    }
    entry.origins = entry.origins.filter((origin) => origin.bucket !== bucket);
    return this.touch(entry);
  }

  invalidate(id: string, paths: string[]): CloudFrontInvalidation {
    const entry = this.find(id);
    const checked = validatePaths(paths);
    if (!checked.ok) {
      fail("usage", checked.message);
    }
    const list = this.invalidations.get(id) ?? [];
    if (list.filter((item) => item.status === "InProgress").length >= 3) {
      fail("conflict", "Too many invalidations are in progress (TooManyInvalidationsInProgress).");
    }
    const created: CloudFrontInvalidation = {
      id: `I${String(1000 + this.counter).padStart(4, "0")}`,
      status: "InProgress",
      create_time: new Date().toISOString(),
      paths: checked.paths,
    };
    this.counter += 1;
    this.invalidations.set(id, [created, ...list]);
    entry.in_progress_invalidations = (entry.in_progress_invalidations ?? 0) + 1;
    return structuredClone(created);
  }

  /** Completes every running invalidation (the mock has no clock of its own). */
  completeAll(): void {
    for (const list of this.invalidations.values()) {
      for (const item of list) item.status = "Completed";
    }
    for (const entry of this.distributions) entry.in_progress_invalidations = 0;
  }

  history(id: string, marker: string | null): InvalidationPage {
    this.find(id);
    const list = this.invalidations.get(id) ?? [];
    const start = marker === null ? 0 : Number(marker);
    const items = list.slice(start, start + HISTORY_PAGE).map((item) => ({ ...structuredClone(item), paths: [] }));
    const next = start + HISTORY_PAGE;
    return { items, next_marker: next < list.length ? String(next) : null };
  }

  invalidation(id: string, invalidationId: string): CloudFrontInvalidation {
    this.find(id);
    const found = (this.invalidations.get(id) ?? []).find((item) => item.id === invalidationId);
    if (found === undefined) {
      fail("not_found", `Invalidation ${invalidationId} does not exist (NoSuchInvalidation).`);
    }
    return structuredClone(found);
  }

  listSets(): PathSet[] {
    return structuredClone(this.sets);
  }

  saveSet(set: PathSet, replaces: string | null): PathSet[] {
    const name = set.name.trim();
    if (name.length === 0) fail("usage", "The path set needs a name.");
    const checked = validatePaths(set.paths);
    if (!checked.ok) fail("usage", checked.message);
    const same = (a: string, b: string) => a.toLowerCase() === b.trim().toLowerCase();
    const at = replaces === null ? -1 : this.sets.findIndex((entry) => same(entry.name, replaces));
    if (replaces !== null && at < 0) fail("not_found", `There is no path set named '${replaces.trim()}'.`);
    const other = this.sets.findIndex((entry) => same(entry.name, name));
    if (other >= 0 && other !== at) fail("conflict", `A path set named '${this.sets[other]?.name ?? name}' already exists.`);
    const next = { name, paths: checked.paths };
    if (at >= 0) this.sets[at] = next;
    else this.sets.push(next);
    return structuredClone(this.sets);
  }

  deleteSet(name: string): PathSet[] {
    const at = this.sets.findIndex((entry) => entry.name.toLowerCase() === name.trim().toLowerCase());
    if (at < 0) fail("not_found", `There is no path set named '${name.trim()}'.`);
    this.sets.splice(at, 1);
    return structuredClone(this.sets);
  }
}
