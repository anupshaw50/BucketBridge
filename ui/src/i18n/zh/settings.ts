/** 简体中文文案（settings）。键是英文原文，值是中文译文。 */
export const zhSettings: Record<string, string> = {
  Interface: "界面",
  Language: "语言",
  "The whole interface switches at once.": "整个界面会立即切换。",
  Theme: "主题",

  // ---- 导航页与标题 ----
  General: "常规",
  Transfers: "传输",
  Advanced: "高级",
  "Data integrity": "数据完整性",
  Logging: "日志",
  Proxy: "代理",
  Sharing: "共享",
  "Settings pages": "设置页",

  // ---- 下拉选项的 label（value 不翻）----
  "None (only when the service requires one)": "无（仅在服务要求时使用）",
  Overwrite: "覆盖",
  "Ask me": "每次询问",
  "Overwrite if newer": "覆盖较新的文件",
  "Rename (name (1).ext)": "重命名（name (1).ext）",
  "Errors only": "仅错误",
  "Warnings and errors": "警告和错误",
  "Everything (information, warnings, errors)": "全部（信息、警告、错误）",

  // ---- 常规 ----
  "Ask before deleting": "删除前询问",
  "Show a confirmation before objects and folders are deleted.": "删除对象和文件夹前先显示确认。",
  "Download folder": "下载文件夹",
  "Ask where to save every time": "每次都询问保存位置",
  "Always download to this folder": "始终下载到该文件夹",
  "Default download folder": "默认下载文件夹",
  "No folder chosen": "未选择文件夹",
  "Browse...": "浏览…",
  "Choose the default download folder": "选择默认下载文件夹",
  "The folder dialog could not be opened": "无法打开文件夹对话框",

  // ---- 界面 ----
  System: "跟随系统",
  Light: "浅色",
  Dark: "深色",
  "List every object below the open folder instead of one folder level.":
    "列出打开的文件夹之下的每个对象，而不是只列一层文件夹。",
  "Show the transfer queue": "显示传输队列",
  "Show the event log": "显示事件日志",

  // ---- 传输 ----
  "Tasks running at the same time": "同时运行的任务数",
  "Parallel parts per task": "每个任务的并行分片数",
  "Multipart threshold": "分片上传阈值",
  "Part size (grows automatically for very large files)": "分片大小（超大文件时自动增大）",
  "Limit bandwidth": "限制带宽",
  "One limit shared by all running transfers.": "所有进行中的传输共用一个限值。",
  "Bandwidth limit": "带宽限制",
  "Retries per part or object": "每个分片或对象的重试次数",
  "When the destination exists": "目标已存在时",
  "What uploads, downloads, copies and moves do with an item that is already there. Each transfer can choose another policy.":
    "上传、下载、复制和移动对已存在项目的处理方式。每次传输可以选择其他策略。",

  // ---- 高级 ----
  "Request attempts (including the first)": "请求尝试次数（含第一次）",
  "Connection timeout": "连接超时",
  "Read timeout": "读取超时",
  "Objects per listing page": "每次列出的对象数",
  seconds: "秒",

  // ---- 数据完整性 ----
  "Upload checksum": "上传校验和",
  "Algorithm used to protect uploads end to end.": "用于端到端保护上传的算法。",
  "Keep modification times": "保留修改时间",
  "Downloaded files get the time of the object, and uploads store the file's time with the object.":
    "下载的文件使用对象的时间；上传时把文件的时间与对象一起存储。",
  "Validate downloads": "校验下载",
  "Compare checksums of downloaded data when the object has one.": "当对象有校验和时，比较下载数据的校验和。",

  // ---- 日志 ----
  Record: "记录",
  "Minimum level written to the event log.": "写入事件日志的最低级别。",
  "Write the log to a file": "把日志写入文件",
  "Maximum log file size": "日志文件大小上限",
  "Log folder: {path}": "日志文件夹：{path}",
  Show: "显示",

  // ---- 代理 ----
  "Connect through a proxy": "通过代理连接",
  "Proxy type": "代理类型",
  Host: "主机",
  Port: "端口",
  "User name (optional)": "用户名（可选）",
  "Bypass the proxy for": "以下地址不走代理",
  "One entry per line: a host name, \"*.example.com\", or a CIDR range such as 10.0.0.0/8.":
    "每行一条：主机名、「*.example.com」，或 10.0.0.0/8 这样的 CIDR 范围。",

  // ---- 代理密码 ----
  Password: "密码",
  "A password is stored in the credential store.": "凭据存储中已保存密码。",
  "No password is stored.": "未保存密码。",
  "Passwords are never shown again; changes here apply immediately.": "密码不会再次显示；此处的更改立即生效。",
  "New proxy password": "新代理密码",
  "Save password": "保存密码",
  "Change password...": "更改密码…",
  "Set password...": "设置密码…",
  "Remove password": "移除密码",
  "Proxy password removed.": "已移除代理密码。",
  "Proxy password saved.": "已保存代理密码。",

  // ---- 对话框整体 ----
  "Settings saved.": "设置已保存。",
  "Some values are not valid. Pages with problems are marked.": "部分数值无效。有问题的页面已标记。",
  Apply: "应用",
  Close: "关闭",
  "The settings could not be loaded{detail}": "设置无法加载{detail}",
};
