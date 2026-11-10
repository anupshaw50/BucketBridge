/** 简体中文文案（tools：诊断、未完成的分片上传、保存的筛选、导出列表）。键是英文原文，值是中文译文。 */
export const zhTools: Record<string, string> = {
  // ---- tools/ToolsMenu.tsx（工具菜单）----
  Tools: "工具",
  "Diagnostics...": "诊断…",
  "Security scan...": "安全扫描…",
  "Uncompleted uploads...": "未完成的分片上传…",
  "Saved filters...": "保存的筛选…",

  // ---- tools/FilterControls.tsx（筛选菜单与筛选标记）----
  "Saved filters": "保存的筛选",
  "Apply a saved filter": "应用保存的筛选",
  "The filters could not be loaded": "无法加载筛选",
  "No saved filters yet": "还没有保存的筛选",
  "{name} (applied)": "{name}（使用中）",
  "Clear saved filter": "清除保存的筛选",
  "Manage filters...": "管理筛选…",
  "Filter: {name}": "筛选：{name}",

  // ---- tools/DiagnosticsDialog.tsx ----
  Diagnostics: "诊断",
  "Add an account first: the diagnostics probe the connection of one account.":
    "请先添加账户：诊断会探测一个账户的连接。",
  "Checks the name lookup, the connection, the proxy, the TLS certificate, the credentials and, when you name a bucket, the region of that bucket, one step after the other with the time each takes.":
    "依次检查名称解析、连接、代理、TLS 证书、凭据，以及（在你填写存储桶名称时）该存储桶的区域，并显示每一步的耗时。",
  Account: "账户",
  "Bucket (optional)": "存储桶（可选）",
  "Adds a check of the region of this bucket.": "会额外检查该存储桶的区域。",

  // ---- tools/DiagnosticsPanel.tsx ----
  skipped: "已跳过",
  "Run diagnostics": "运行诊断",
  "Run again": "再次运行",
  "Copy report": "复制报告",
  "Running diagnostics...": "正在运行诊断…",
  "{target} ({region}{extras}), {time} in total.": "{target}（{region}{extras}），总计 {time}。",
  ", bucket {bucket}": "，存储桶 {bucket}",
  "Diagnostic steps": "诊断步骤",
  "Diagnostics report copied to the clipboard.": "诊断报告已复制到剪贴板。",
  "The report could not be copied": "无法复制报告",

  // ---- tools/UploadsDialog.tsx（未完成的分片上传）----
  "Uncompleted multipart uploads": "未完成的分片上传",
  "Uncompleted uploads": "未完成的上传",
  "Add or choose an account first.": "请先添加或选择一个账户。",
  "An upload that was started and never finished keeps the parts it uploaded, and they are billed until the upload is aborted. Choose buckets to see their unfinished uploads.":
    "已开始但从未完成的上传会保留其已上传的分片，在中止之前这些分片一直计费。请选择存储桶查看其未完成的上传。",
  "Choose buckets": "选择存储桶",
  "No bucket with that name": "没有该名称的存储桶",
  "All buckets": "所有存储桶",
  "List uploads": "列出上传",
  Refresh: "刷新",
  "Could not list {bucket}": "无法列出 {bucket}",
  "No uncompleted uploads in the chosen buckets.": "所选存储桶中没有未完成的上传。",
  "Older than (days)": "早于（天）",
  "Select older": "选择更早的",
  "1 upload selected of {count}": "已选择 1 个上传，共 {count} 个",
  "{count} uploads selected of {total}": "已选择 {count} 个上传，共 {total} 个",
  "Abort selected ({count})": "中止所选（{count}）",
  "Select all uploads": "全选上传",
  "Select {key} in {bucket}": "选择 {bucket} 中的 {key}",
  Bucket: "存储桶",
  Key: "键",
  Initiated: "开始时间",
  Age: "时长",
  "Storage class": "存储类型",
  "Results of the last abort": "最近一次中止的结果",
  "Abort results": "中止结果",
  Result: "结果",
  Aborted: "已中止",
  "Abort uploads": "中止上传",
  "Abort 1 uncompleted upload? The parts they uploaded so far are deleted and their storage is freed. This cannot be undone.":
    "要中止这 1 个未完成的上传吗？它已上传的分片将被删除，占用的存储将被释放。此操作无法撤销。",
  "Abort {count} uncompleted uploads? The parts they uploaded so far are deleted and their storage is freed. This cannot be undone.":
    "要中止这 {count} 个未完成的上传吗？它们已上传的分片将被删除，占用的存储将被释放。此操作无法撤销。",
  "1 upload aborted.": "已中止 1 个上传。",
  "{count} uploads aborted.": "已中止 {count} 个上传。",
  "1 upload aborted, {count} could not be aborted.": "已中止 1 个上传，{count} 个无法中止。",
  "{count} uploads aborted, {failed} could not be aborted.": "已中止 {count} 个上传，{failed} 个无法中止。",

  // ---- tools/ExportDialog.tsx（导出列表）----
  "Plain text for any tool or script. Text that starts with = + - or @ gets a leading apostrophe.":
    "任何工具或脚本都能读取的纯文本。以 = + - 或 @ 开头的文本会加一个前导单引号。",
  "An Excel workbook with a header row. A listing past 1,048,576 rows continues on more sheets.":
    "带表头行的 Excel 工作簿。列表超过 1,048,576 行时会写到更多工作表。",
  "A compact columnar file for data tools, with Snappy compression.": "面向数据分析工具的紧凑列式文件，采用 Snappy 压缩。",
  "The file dialog could not be opened": "无法打开文件对话框",
  "Exported 1 row to {path}.": "已导出 1 行到 {path}。",
  "Exported {count} rows to {path}.": "已导出 {count} 行到 {path}。",
  "The folder could not be shown": "无法显示该文件夹",
  "Export finished": "导出完成",
  "Exported 1 row ({size}) to {path}.": "已导出 1 行（{size}）到 {path}。",
  "Exported {count} rows ({size}) to {path}.": "已导出 {count} 行（{size}）到 {path}。",
  "Show in folder": "在文件夹中显示",
  "Writes the listing of": "把",
  "to a file with the columns key, size, last_modified, etag and storage_class.":
    "的列表写入文件，列包括键、大小、last_modified、etag 和 storage_class。",
  "File format": "文件格式",
  "Include everything below this folder": "包含此文件夹下的全部内容",
  "Off: only what the folder view shows, one level.": "关闭：仅包含文件夹视图显示的一层内容。",
  "Only entries matching the active filter \"{name}\"": "仅导出与当前筛选「{name}」匹配的条目",
  "Export to file...": "导出到文件…",

  // ---- tools/FiltersDialog.tsx（保存的筛选）----
  "{label} unit": "「{label}」的单位",
  "Matches {kept} of {total} items loaded in s3://{bucket}/{prefix}.":
    "在已加载的 s3://{bucket}/{prefix} 的 {total} 个项目中匹配 {kept} 个。",
  "Filter \"{name}\" saved.": "筛选「{name}」已保存。",
  "New filter": "新建筛选",
  "Edit filter {name}": "编辑筛选 {name}",
  "Filter name": "筛选名称",
  Prefix: "前缀",
  "The path below the folder you browse starts with this text.": "你浏览的文件夹之下的路径需以该文本开头。",
  "Glob pattern": "通配符模式",
  "For example **/*.jpg. * stays within a folder, ** crosses folders.": "例如 **/*.jpg。* 不跨文件夹，** 可跨文件夹。",
  "Minimum size": "最小大小",
  "Maximum size": "最大大小",
  "Modified on or after": "修改时间不早于",
  "Modified before": "修改时间早于",
  "Storage classes": "存储类型",
  "Any storage class": "任意存储类型",
  "Save filter": "保存筛选",
  "Delete filter": "删除筛选",
  "Delete the saved filter \"{name}\"?": "要删除保存的筛选「{name}」吗？",
  "Filter \"{name}\" deleted.": "筛选「{name}」已删除。",
  "The filter could not be deleted": "无法删除该筛选",
  "A saved filter narrows the object list to the items you have loaded that match it: a prefix or glob pattern, a size range, a modification date range and storage classes. Pick one from the Filter menu of the toolbar.":
    "保存的筛选会把对象列表缩小到你已加载且与之匹配的项目：前缀或通配符模式、大小范围、修改时间范围和存储类型。可从工具栏的筛选菜单中选用。",
  "No saved filters yet.": "还没有保存的筛选。",
  "Stored filters": "已保存的筛选",
  "What it matches": "匹配条件",
  Applied: "使用中",
  "Apply filter {name}": "应用筛选 {name}",
  "Delete filter {name}": "删除筛选 {name}",
  // ---- 恢复未完成的上传 ----
  "Resume...": "恢复...",
  "Resume upload": "恢复上传",
  "Choose the file this upload was made from": "选择此上传所用的文件",
  "Choose another file...": "选择其他文件...",
  "This file is smaller than the part of the upload that is already on the server, so it is not the file the upload was made from.":
    "此文件比服务器上已有的上传部分还小，所以它不是该上传所用的文件。",
  "The parts on the server do not fit this file, so it is not the file the upload was made from.":
    "服务器上的分片与此文件不匹配，所以它不是该上传所用的文件。",
  "Part {part} on the server is not the same as this file, so it is not the file the upload was made from.":
    "服务器上的第 {part} 个分片与此文件不一致，所以它不是该上传所用的文件。",
  "Choose the file this upload was made from. It is compared with the parts already on the server, and only the missing parts are uploaded.":
    "选择此上传所用的文件。它会与服务器上已有的分片比对，只上传缺少的分片。",
  "Upload to s3://{bucket}/{key}": "上传到 s3://{bucket}/{key}",
  "This file cannot finish the upload": "此文件无法完成该上传",
  "This file matches the upload": "此文件与该上传一致",
  "{count} of {total} parts are already uploaded ({size}).": "已上传 {count} / {total} 个分片（{size}）。",
  "{count} parts remain to upload ({size}).": "还需上传 {count} 个分片（{size}）。",
  "The content of the uploaded parts was not compared; their sizes and positions fit.":
    "未比对已上传分片的内容；它们的大小和位置相符。",
  "The uploaded parts were compared with the file.": "已上传的分片已与文件比对。",
  "Do not compare the content of the uploaded parts with the file": "不将已上传分片的内容与文件比对",
  "Use this when the upload is encrypted with a KMS key or a customer key and has no checksums: the parts then cannot be compared. Only the sizes and positions are checked.":
    "当上传使用 KMS 密钥或客户密钥加密且没有校验和时使用：此时无法比对分片，只检查大小和位置。",
  "The upload of {key} continues in the transfers list.": "{key} 的上传将在传输列表中继续。",
};
