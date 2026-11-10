/** 简体中文文案（buckets）。键是英文原文，值是中文译文。 */
export const zhBuckets: Record<string, string> = {
  // ---- BucketTree.tsx ----
  "Failed to load.": "加载失败。",
  "Filter buckets": "筛选存储桶",
  "Filter by region and creation date": "按区域和创建日期筛选",
  Region: "区域",
  "Any region": "任意区域",
  "Created on or after": "创建日期不早于",
  "Created on or before": "创建日期不晚于",
  "{matching} of {total} buckets match": "{total} 个存储桶中有 {matching} 个匹配",
  "Clear filters": "清除筛选",
  "Showing {shown} of {total} buckets": "显示 {shown} 个，共 {total} 个存储桶",
  "Show more buckets": "显示更多存储桶",
  "Loading buckets": "正在加载存储桶",
  "Buckets unavailable": "存储桶不可用",
  Unlock: "解锁",
  "This account has no buckets yet.": "该账户还没有存储桶。",
  "Create bucket": "创建存储桶",
  "Refreshing buckets": "正在刷新存储桶",
  "New bucket": "新建存储桶",
  "Reload buckets": "重新加载存储桶",
  "Buckets and folders": "存储桶和文件夹",
  "Add external bucket": "添加外部存储桶",
  "Remove external bucket": "移除外部存储桶",
  "Remove external bucket {name}": "移除外部存储桶 {name}",
  External: "外部",
  "Loading folders...": "正在加载文件夹…",
  "Loading...": "加载中…",
  "Load more folders": "加载更多文件夹",
  "Could not load folders. Select to retry.": "无法加载文件夹。选中后重试。",
  // ---- BucketDialogs.tsx ----
  "Bucket {name} created.": "已创建存储桶 {name}。",
  "Bucket {name} deleted.": "已删除存储桶 {name}。",
  "Delete bucket": "删除存储桶",
  "3 to 63 characters: lowercase letters, digits, dots and hyphens.":
    "3 至 63 个字符：小写字母、数字、点和连字符。",
  "Region (optional)": "区域（可选）",
  "Leave empty to use the account region{details}.": "留空则使用账户区域{details}。",
  "The bucket": "该存储桶",
  "is deleted permanently. Only empty buckets can be deleted.": "会被永久删除。只有空存储桶才能删除。",
  "Delete all objects in the bucket first, then try again.": "请先删除存储桶中的所有对象，然后重试。",

  // ---- 高级选项 ----
  "Server default": "服务器默认",
  "Bucket owner enforced (ACLs disabled, recommended)": "强制存储桶所有者（禁用 ACL，推荐）",
  "Bucket owner preferred (ACLs enabled)": "存储桶所有者优先（启用 ACL）",
  "Object writer (ACLs enabled)": "对象写入者（启用 ACL）",
  "Object Lock cannot be turned off again once {name} is created, and S3 turns versioning on for it. Objects can then be protected from deletion.":
    "{name} 创建后，对象锁定无法再关闭，并且 S3 会为它开启版本控制。之后可以保护对象不被删除。",
  "Bucket {name} was created, with a warning": "存储桶 {name} 已创建，但有一条警告",
  "Advanced options": "高级选项",
  "Enable Object Lock": "启用对象锁定",
  "Lets you protect objects from being deleted or overwritten. It cannot be turned off later, and S3 turns versioning on with it.":
    "可保护对象不被删除或覆盖。之后无法关闭，并且 S3 会同时开启版本控制。",
  "Object ownership": "对象所有权",
  "Who owns uploaded objects. With ACLs disabled the bucket owner owns everything and access is controlled by policies.":
    "谁拥有上传的对象。禁用 ACL 时，存储桶所有者拥有全部对象，访问由策略控制。",
  "Block public access": "阻止公共访问",
  "With a switch off, this bucket can become public through ACLs or a bucket policy.":
    "有开关关闭时，该存储桶可能通过 ACL 或存储桶策略变为公开。",
  "A server that does not support the public access block leaves it out and tells you.":
    "不支持公共访问封锁的服务器会跳过它，并告知你。",
};
