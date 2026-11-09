/** 简体中文文案（accounts）。键是英文原文，值是中文译文。 */
export const zhAccounts: Record<string, string> = {
  // ---- AccountFields.tsx（连接字段）----
  Label: "标签",
  "A unique name for this account.": "该账户的唯一名称。",
  Endpoint: "端点",
  "Leave empty for AWS. For other S3-compatible services enter the endpoint URL.":
    "使用 AWS 时留空。其他 S3 兼容服务请输入端点 URL。",
  Region: "区域",
  "Use path-style addressing": "使用路径风格（path-style）寻址",
  "Needed by some S3-compatible services that do not support bucket host names.":
    "部分不支持存储桶主机名的 S3 兼容服务需要开启此项。",
  "Requester pays": "请求方付费",
  "Accept charges when reading from requester-pays buckets.": "从请求方付费存储桶读取时，接受相应的费用。",
  "Access key ID": "访问密钥 ID",
  "Secret access key": "秘密访问密钥",
  "Leave empty to keep the stored secret. Enter a new one to rotate it.":
    "留空则保留已存储的秘密访问密钥。输入新密钥可将其轮换。",
  Unchanged: "未更改",
  "Show or hide the secret access key": "显示或隐藏秘密访问密钥",
  "Session token": "会话令牌",
  Keep: "保留",
  Replace: "替换",
  Remove: "移除",
  "Add a session token": "添加会话令牌",
  "New session token": "新会话令牌",
  "Session token (optional)": "会话令牌（可选）",
  "Only for temporary credentials.": "仅临时凭据需要。",
  "Show or hide the session token": "显示或隐藏会话令牌",
  "MFA device (optional)": "MFA 设备（可选）",
  "ARN or serial number of the MFA device. You will be asked for a code when needed.":
    "MFA 设备的 ARN 或序列号。需要时会向你索要验证码。",

  // ---- accountActions.ts（移除账户、设为默认）----
  "Remove account": "移除账户",
  "Remove \"{label}\" and delete its stored secrets? Transfers of this account that are still queued will fail.":
    "要移除「{label}」并删除其存储的凭据吗？该账户仍在排队的传输将会失败。",
  "Account \"{label}\" removed.": "已移除账户「{label}」。",
  "The account could not be removed": "无法移除该账户",
  "\"{label}\" is now the default account.": "「{label}」已是默认账户。",
  "The default account could not be changed": "无法更改默认账户",

  // ---- AccountDialogs.tsx（添加账户向导）----
  "Add account": "添加账户",
  Connection: "连接",
  Credentials: "凭据",
  Review: "确认信息",
  "Test connection": "测试连接",
  "Check the details and add the account. The secret is stored in the credential store.":
    "核对信息后添加账户。秘密访问密钥会保存在凭据存储中。",
  "Check the details and add the account. No secret is stored: credentials are requested when needed.":
    "核对信息后添加账户。不保存任何秘密访问密钥：凭据会在需要时再请求。",
  "Path-style addressing": "路径风格（path-style）寻址",
  Yes: "是",
  No: "否",
  "Entered (hidden)": "已输入（隐藏）",
  None: "无",
  "MFA device": "MFA 设备",
  "Assumed role": "已代入的角色",
  "Signed with": "签名账户",
  "Role ARN": "角色 ARN",
  Session: "会话",
  "{name}, {n} minutes": "{name}，{n} 分钟",
  "External ID": "外部 ID",
  "AWS SSO (IAM Identity Center)": "AWS SSO（IAM Identity Center）",
  "Start URL": "起始 URL",
  "AWS account": "AWS 账户",
  Role: "角色",
  "Role of this EC2 instance or container": "这台 EC2 实例或容器的角色",
  "Account \"{label}\" added.": "已添加账户「{label}」。",

  // ---- AccountDialogs.tsx（编辑账户）----
  "Edit account": "编辑账户",
  Close: "关闭",
  Save: "保存",
  "This account no longer exists.": "该账户已不存在。",
  "Edit account {label}": "编辑账户 {label}",
  "Account \"{label}\" saved.": "已保存账户「{label}」。",

  // ---- AccountDialogs.tsx（账户管理）----
  Accounts: "账户",
  "A master password protects the stored secrets. They are {state}.": "主密码保护着已存储的凭据。它们当前{state}。",
  unlocked: "已解锁",
  locked: "已锁定",
  "No master password is set. Secrets are protected by the operating system's credential store only.":
    "尚未设置主密码。凭据仅由操作系统的凭据存储保护。",
  "Lock now": "立即锁定",
  Unlock: "解锁",
  "Master password...": "主密码…",
  "No accounts yet.": "还没有账户。",
  Service: "服务",
  Actions: "操作",
  Default: "默认",
  Active: "使用中",
  "MFA device {serial}": "MFA 设备 {serial}",
  "Use {label}": "使用 {label}",
  Use: "使用",
  "Make {label} the default": "将 {label} 设为默认",
  "Set default": "设为默认",
  "Edit {label}": "编辑 {label}",
  Edit: "编辑",
  "Remove {label}": "移除 {label}",
  "Add account...": "添加账户…",
  "Import from AWS config...": "从 AWS 配置导入…",
  "Secrets locked.": "凭据已锁定。",

  // ---- ImportAwsDialog.tsx（从 AWS 配置导入）----
  "Access keys": "访问密钥",
  "Credential process": "凭据处理程序",
  "Web identity": "Web 身份",
  Incomplete: "不完整",
  "Imported {n} AWS profile.": "已导入 {n} 个 AWS 配置文件。",
  "Imported {n} AWS profiles.": "已导入 {n} 个 AWS 配置文件。",
  "Import from AWS config": "从 AWS 配置导入",
  "No profile was imported.": "没有导入任何配置文件。",
  "Imported {n} profile: {names}.": "已导入 {n} 个配置文件：{names}。",
  "Imported {n} profiles: {names}.": "已导入 {n} 个配置文件：{names}。",
  "Left out": "已跳过",
  Import: "导入",
  Profile: "配置文件",
  Kind: "类型",
  Status: "状态",
  "Reading the AWS files": "正在读取 AWS 文件",
  "No profiles were found in the AWS config files.": "在 AWS 配置文件中没有找到任何配置文件。",
  "AWS profiles": "AWS 配置文件",
  "Import {name}": "导入 {name}",
  "Already imported": "已导入过",
  "Choose profiles from your AWS config and credentials files. The accounts stay linked to the files: no key is copied into BucketBridge, and changes you make in the files apply the next time credentials are needed.":
    "请从你的 AWS 配置和凭据文件中选择要导入的配置文件。账户将持续关联这些文件：任何密钥都不会复制进 BucketBridge，你对文件所作的更改会在下次需要凭据时生效。",

  // ---- MasterPasswordDialog.tsx（主密码）----
  "Password strength: {label}": "密码强度：{label}",
  Strength: "强度",
  "Password generator": "密码生成器",
  Length: "长度",
  "Include symbols": "包含符号",
  Generate: "生成",
  "Generated password": "生成的密码",
  "Use this password": "使用此密码",
  "Use at least {n} characters.": "请至少使用 {n} 个字符。",
  "The passwords do not match.": "两次输入的密码不一致。",
  "Enter the current master password.": "请输入当前的主密码。",
  "Master password removed.": "已移除主密码。",
  "Master password changed.": "已更改主密码。",
  "Master password set.": "已设置主密码。",
  "The master password is not correct. Try again.": "主密码不正确。请重试。",
  "The current master password is not correct.": "当前的主密码不正确。",

  // ---- MasterPasswordDialog.tsx（设置、更改、移除主密码）----
  "Master password": "主密码",
  "Set a master password": "设置主密码",
  "Remove password": "移除密码",
  "Change password": "更改密码",
  "Set password": "设置密码",
  Change: "更改",
  Action: "操作",
  "Current master password": "当前主密码",
  "New master password": "新主密码",
  "Repeat the new password": "再次输入新密码",
  "The master password encrypts the stored account secrets. You enter it once per session. If you forget it, the secrets cannot be recovered and the accounts have to be added again.":
    "主密码会加密已存储的账户凭据。每个会话只需输入一次。如果忘记主密码，凭据将无法恢复，账户需要重新添加。",
  "Without a master password the secrets stay protected by the operating system's credential store only.":
    "没有主密码时，凭据仅由操作系统的凭据存储保护。",

  // ---- SessionPrompts.tsx（解锁与 MFA 提示）----
  "Secrets unlocked.": "凭据已解锁。",
  "Stored credentials are protected by a master password. Enter it to use your accounts.":
    "已存储的凭据由主密码保护。请输入主密码以使用你的账户。",
  "MFA session started for {label}.": "已为 {label} 启动 MFA 会话。",
  "MFA session started for the account.": "已为该账户启动 MFA 会话。",
  "Multi-factor authentication": "多因素身份验证",
  Verify: "验证",
  "requires a code from its MFA device.": "需要来自其 MFA 设备的验证码。",
  "requires a code from its MFA device ({serial}).": "需要来自其 MFA 设备（{serial}）的验证码。",
  "This account requires a code from its MFA device.": "该账户需要来自其 MFA 设备的验证码。",
  "MFA code": "MFA 验证码",
  "Six digits": "六位数字",

  // ---- SourceFields.tsx（凭据来源选择）----
  "Credentials from": "凭据来源",
  "Assume a role": "代入角色",
  "Instance role": "实例角色",
  "An access key and secret key, kept in the operating system's credential store.":
    "访问密钥与秘密访问密钥，保存在操作系统的凭据存储中。",
  "Temporary credentials for an IAM role, requested with another account of yours. Nothing secret is stored.":
    "用你的另一个账户为 IAM 角色请求临时凭据。不保存任何秘密访问密钥。",
  "Sign in to IAM Identity Center (AWS SSO) in your browser and choose an account and role. Nothing secret is stored.":
    "在浏览器中登录 IAM Identity Center（AWS SSO），然后选择账户和角色。不保存任何秘密访问密钥。",
  "The role of the EC2 instance or ECS or EKS container BucketBridge runs on. Nothing secret is stored.":
    "BucketBridge 所在的 EC2 实例或 ECS、EKS 容器的角色。不保存任何秘密访问密钥。",
  "A profile of your AWS config files. The files stay the source of the credentials.":
    "AWS 配置文件中的一个配置文件。这些文件仍是凭据的来源。",

  // ---- SourceFields.tsx（代入角色字段）----
  "Sign the request with": "请求签名账户",
  "The account whose credentials call STS AssumeRole. It can be any account, including another role.":
    "用该账户的凭据调用 STS AssumeRole。可以是任何账户，包括另一个角色。",
  "Session name": "会话名称",
  "Session length (minutes)": "会话时长（分钟）",
  "Credentials refresh before they expire.": "凭据会在过期前自动刷新。",
  "External ID (optional)": "外部 ID（可选）",
  "Only if the role's trust policy asks for one.": "仅当角色的信任策略要求时才需要填写。",
  "ARN or serial number of the device the role's trust policy asks for. You enter a code each time the session ends.":
    "角色信任策略所要求设备的 ARN 或序列号。每次会话结束时都需要输入验证码。",

  // ---- SourceFields.tsx（实例角色）----
  container: "容器",
  instance: "实例",
  "Check this machine": "检查这台机器",
  "Found the role of this {machine} (access key {accessKeyId}).":
    "找到了这台{machine}的角色（访问密钥 {accessKeyId}）。",
  "BucketBridge asks the EC2 instance metadata service (version 2) or the container credentials endpoint for the role of this machine, and refreshes the credentials before they expire. This only works on an EC2 instance or an ECS or EKS container.":
    "BucketBridge 会向 EC2 实例元数据服务（版本 2）或容器凭据端点查询这台机器的角色，并在凭据过期前刷新它们。该方式仅适用于 EC2 实例或 ECS、EKS 容器。",
  "This account uses the profile {profile} of your AWS config files. Change keys, roles or SSO settings in those files; BucketBridge reads them whenever it needs credentials.":
    "此账户使用你 AWS 配置文件中的配置文件 {profile}。请直接在这些文件中修改密钥、角色或 SSO 设置；BucketBridge 每次需要凭据时都会读取它们。",

  // ---- SourceFields.tsx（IAM Identity Center / AWS SSO）----
  "SSO region": "SSO 区域",
  "The AWS access portal URL of your organization.": "你所属组织的 AWS 访问门户 URL。",
  "The region IAM Identity Center runs in.": "IAM Identity Center 所在的区域。",
  "Session name (optional)": "会话名称（可选）",
  "The sso-session of your AWS config, if you use one.": "AWS 配置中的 sso-session（如果你有配置的话）。",
  "Enter the portal start URL (https://...) and the region first.": "请先填写门户起始 URL（https://…）和区域。",
  "Sign in with AWS SSO": "使用 AWS SSO 登录",
  "Sign in again": "重新登录",
  "Cancel sign-in": "取消登录",
  "Confirm the sign-in in your browser": "请在浏览器中确认此次登录",
  "Enter this code if the page asks for it:": "如果页面要求输入代码，请输入：",
  "The page was opened in your browser.": "该页面已在你的浏览器中打开。",
  "Open this page in your browser:": "请在浏览器中打开这个页面：",
  "Waiting for you to approve...": "等待你批准…",
  "Signed in. The token is kept in the AWS CLI cache (~/.aws/sso/cache), not in BucketBridge.":
    "已登录。令牌保存在 AWS CLI 缓存（~/.aws/sso/cache）中，而不是 BucketBridge 里。",
  "Sign in first": "请先登录",
  "Choose an account first": "请先选择账户",
  "Choose a role": "请选择角色",
  "Account {account}, role {role}.": "账户 {account}，角色 {role}。",
  "To use another role, add a new account.": "如需使用其他角色，请添加新账户。",

  // ---- IAMDialog 等（用户名占位）----
  "user name": "用户名",
};
