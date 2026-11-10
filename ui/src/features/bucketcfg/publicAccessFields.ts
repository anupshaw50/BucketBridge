import type { PublicAccessBlock } from "../../api/types";

/** The four toggles of the public access block with their texts (also used when a bucket is created). */
export const FIELDS: { key: keyof PublicAccessBlock; label: string; description: string }[] = [
  {
    key: "block_public_acls",
    label: "Block new public ACLs",
    description: "Requests that would make the bucket or an object public through an ACL are rejected (BlockPublicAcls).",
  },
  {
    key: "ignore_public_acls",
    label: "Ignore public ACLs",
    description: "Public ACLs that already exist have no effect (IgnorePublicAcls).",
  },
  {
    key: "block_public_policy",
    label: "Block new public bucket policies",
    description: "A bucket policy that would make the bucket public is rejected (BlockPublicPolicy).",
  },
  {
    key: "restrict_public_buckets",
    label: "Restrict access to buckets with a public policy",
    description:
      "A public policy only gives access to the owner's account and to AWS services (RestrictPublicBuckets).",
  },
];
