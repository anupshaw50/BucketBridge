/** 简体中文文案（buckets）。键是英文原文，值是中文译文。 */
export const zhBuckets: Record<string, string> = {
  // ---- BucketTree.tsx ----
  "Failed to load.": "加载失败。",
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
};
