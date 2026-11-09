//! Simplified-Chinese localization for the command-line interface:
//! the English source is kept everywhere in the code, and [`tr`] /
//! [`trf`] turn it into Chinese at run time.
//!
//! The language follows the system: Chinese only when `BB_LANG`, or else
//! `LC_ALL`, `LC_MESSAGES`, or `LANG`, begins with `zh`; in every other
//! case (including no locale at all) the output stays English and [`tr`]
//! returns its argument unchanged.
//!
//! Two kinds of lookups share one table:
//!
//! * help texts, translated by [`translate_command`], which walks the
//!   whole clap command tree and replaces every help string it finds;
//! * user-visible output of the commands themselves, wrapped at the
//!   call sites (`format!` texts with values become `{name}` placeholders
//!   filled by [`trf`]).
//!
//! Before a lookup, both sides are normalized: runs of whitespace
//! (including the hard line breaks of the help sources) collapse to one
//! space, so a help text written on several lines matches a
//! single-line entry. Translation values may still contain newlines.
//!
//! Not translated: JSON keys and enum values of `--json` records,
//! `tracing`/log messages, internal error details, and everything from
//! `bucketbridge_core` (the backend messages are out of scope). Brand
//! and technical names (S3, AWS, CloudFront, IAM, SSE-C, ...) and
//! option or environment-variable names stay as they are inside the
//! Chinese texts.

use std::collections::HashMap;
use std::sync::OnceLock;

/// How many entries the English→Chinese table contains.
pub fn entry_count() -> usize {
    table().len()
}

/// Whether the interface should speak English.
pub fn english() -> bool {
    #[cfg(test)]
    {
        if let Some(forced) = TEST_OVERRIDE.with(std::cell::Cell::get) {
            return forced;
        }
    }
    *CACHE.get_or_init(env_english)
}

#[cfg(test)]
thread_local! {
    static TEST_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}
static CACHE: OnceLock<bool> = OnceLock::new();

/// The environment decides once per process: Chinese only when `BB_LANG`
/// wins, or else `LC_ALL`/`LC_MESSAGES`/`LANG`, with a `zh` locale;
/// everything else (including no locale at all) is English.
fn env_english() -> bool {
    english_from(|name| std::env::var(name).ok())
}

/// [`env_english`] over an arbitrary variable source, so the precedence can
/// be tested without touching the process environment.
fn english_from(var: impl Fn(&str) -> Option<String>) -> bool {
    if let Some(lang) = var("BB_LANG") {
        return !lang_is_chinese(&lang);
    }
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(lang) = var(name) {
            return !lang_is_chinese(&lang);
        }
    }
    true
}

#[cfg(test)]
pub(crate) fn with_language<T>(english: bool, body: impl FnOnce() -> T) -> T {
    let previous = TEST_OVERRIDE.with(std::cell::Cell::get);
    TEST_OVERRIDE.with(|cell| cell.set(Some(english)));
    let out = body();
    TEST_OVERRIDE.with(|cell| cell.set(previous));
    out
}

fn lang_is_chinese(lang: &str) -> bool {
    lang.trim().to_ascii_lowercase().starts_with("zh")
}

/// Translation table, shared by help texts and output texts.
fn table() -> &'static HashMap<String, &'static str> {
    static TABLE: OnceLock<HashMap<String, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| {
        std::iter::once(&AFTER_ENTRY)
            .chain(HELP.iter())
            .chain(OUTPUT.iter())
            .map(|(source, target)| (normalize(source), *target))
            .collect()
    })
}

/// `English source → simplified Chinese`. Unregistered text passes
/// through (blanks collapse first, so a wrapped source and its entry
/// need not share the same line breaks). As the short help of an
/// argument drops the trailing period of the doc comment, the lookup
/// falls back to the entry one period longer.
pub fn tr(source: &str) -> String {
    if english() {
        return source.to_owned();
    }
    let key = normalize(source);
    if let Some(target) = table().get(&key) {
        return (*target).to_owned();
    }
    if let Some(target) = table().get(&format!("{key}.")) {
        return (*target).to_owned();
    }
    source.to_owned()
}

/// [`tr`], with `{name}` placeholders filled afterwards: the values go
/// into the Chinese text at the position the translation reserves for
/// them. When a value itself contains a placeholder it is not replaced.
pub fn trf(source: &str, vars: &[(&str, &str)]) -> String {
    let text = tr(source);
    // One pass over the template: a value that itself contains `{name}`
    // (an S3 key can) is inserted as it is, never expanded again.
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        match vars.iter().find(|(key, _)| *key == name) {
            Some((_, value)) => out.push_str(value),
            None => {
                out.push('{');
                out.push_str(name);
                out.push('}');
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Collapses runs of whitespace (newlines, tabs, several blanks) to one
/// space and trims the edges; keys and lookups both go through it.
fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Replaces every help text of `cmd` (and of its whole subcommand
/// tree) with its translation. Unregistered texts stay as they are,
/// and in English mode this changes nothing at all.
pub fn translate_command(cmd: &mut clap::Command) {
    if english() {
        return;
    }
    let about = cmd.get_about().map(|text| text.to_string());
    if let Some(text) = about {
        *cmd = std::mem::take(cmd).about(tr(&text));
    }
    let long_about = cmd.get_long_about().map(|text| text.to_string());
    if let Some(text) = long_about {
        *cmd = std::mem::take(cmd).long_about(tr(&text));
    }
    let after_help = cmd.get_after_help().map(|text| text.to_string());
    if let Some(text) = after_help {
        *cmd = std::mem::take(cmd).after_help(tr(&text));
    }
    let after_long_help = cmd.get_after_long_help().map(|text| text.to_string());
    if let Some(text) = after_long_help {
        *cmd = std::mem::take(cmd).after_long_help(tr(&text));
    }
    *cmd = std::mem::take(cmd).mut_args(|arg| {
        let help = arg.get_help().map(|text| tr(&text.to_string()));
        let long_help = arg.get_long_help().map(|text| tr(&text.to_string()));
        let mut arg = arg;
        if let Some(help) = help {
            arg = arg.help(help);
        }
        if let Some(long_help) = long_help {
            arg = arg.long_help(long_help);
        }
        arg
    });
    for sub in cmd.get_subcommands_mut() {
        translate_command(sub);
    }
}

/// Phrases of clap's own rendered errors, replaced for a Chinese
/// rendering (the error text goes to [`crate::app::clap_exit`] before
/// being printed). Values inside `'…'` are kept as they are.
const CLAP_PHRASES: &[(&str, &str)] = &[
    ("error: ", "错误："),
    (
        "the following required arguments were not provided:",
        "缺少以下必需参数：",
    ),
    ("unrecognized subcommand '", "无法识别的子命令 '"),
    ("' found", "'（不存在）"),
    ("unexpected argument '", "意外的参数 '"),
    (
        "' requires a subcommand but one was not provided",
        "' 需要一个子命令，但未提供任何子命令",
    ),
    ("the argument '", "参数 '"),
    ("' cannot be used multiple times", "' 不能重复使用"),
    ("' cannot be used with", "' 不能与以下参数一起使用"),
    ("the subcommand '", "子命令 '"),
    ("a value is required for '", "'需要提供值：'"),
    ("invalid value '", "值无效：'"),
    ("' for '", "'，用于 '"),
    ("possible values:", "可选值："),
    ("tip: ", "提示："),
    (
        "For more information, try '--help'.",
        "更多信息请运行 '--help'。",
    ),
    // The skeleton clap prints around the help text.
    ("Usage: ", "用法："),
    ("Commands:", "命令："),
    ("Options:", "选项："),
    ("Arguments:", "参数："),
    ("Print help", "显示帮助"),
    ("Print version", "显示版本"),
];

/// Applies [`CLAP_PHRASES`] to a rendered clap error; best effort.
pub fn translate_clap_error(text: &str) -> String {
    if english() {
        return text.to_owned();
    }
    let mut out = text.to_owned();
    for (phrase, replacement) in CLAP_PHRASES {
        if out.contains(phrase) {
            out = out.replace(phrase, replacement);
        }
    }
    out
}

// ---- the table ----
// Sections: (1) AFTER_HELP, (2) clap help texts, (3) command output.
// The help texts and the output texts live in the two included files
// (as the `HELP` / `OUTPUT` tables) so that this file stays readable.

// (1) The `after_help` block of `bb`. Note the trailing spaces of the
// aligned columns: they collapse away in the lookup.
#[doc(hidden)]
pub const AFTER_ENTRY: (&str, &str) = (        "Accounts: --account picks a stored account by label or ID; without it the \
default account is used, and without one the environment account: \
BB_ACCESS_KEY_ID      access key ID \
BB_SECRET_ACCESS_KEY  secret access key \
BB_SESSION_TOKEN      session token for temporary credentials \
BB_REGION             signing region (default us-east-1) \
BB_ENDPOINT           custom endpoint URL for S3-compatible providers \
BB_PATH_STYLE         use path-style addressing (true/false) \
Other variables: \
BB_MASTER_PASSWORD    master password for stored secrets (else asked on the terminal) \
BB_CSE_PASSWORD       password of --encrypt / --decrypt and bb encrypt / decrypt (else --password-file, else asked on the terminal) \
BB_CONFIG_DIR         directory holding config.toml (same as --config-dir) \
BB_SHORTENER_TOKEN    API token of the URL shortener for `presign --shorten` \
Exit codes: 0 ok, 1 usage error, 2 remote or local failure (also not found and \
conflicts), 3 authentication failure or locked secrets, 130 interrupted.",
        "账户：--account 按标签或 ID 选择已存储的账户；不带它时使用默认账户，
再没有时使用由环境变量构成的 environment 账户：
  BB_ACCESS_KEY_ID      访问密钥 ID
  BB_SECRET_ACCESS_KEY  秘密访问密钥
  BB_SESSION_TOKEN      临时凭据的会话令牌
  BB_REGION             签名区域（默认 us-east-1）
  BB_ENDPOINT           S3 兼容服务商的自定义端点 URL
  BB_PATH_STYLE         使用路径风格寻址（true/false）
其他变量：
  BB_MASTER_PASSWORD    已存储机密的主密码（否则在终端询问）
  BB_CSE_PASSWORD       --encrypt / --decrypt 以及 bb encrypt / decrypt 的密码（否则用 --password-file，再否则在终端询问）
  BB_CONFIG_DIR         存放 config.toml 的目录（同 --config-dir）
  BB_SHORTENER_TOKEN    用于 `presign --shorten` 的短链服务 API 令牌

退出码：0 成功，1 用法错误，2 远端或本地失败（也包括未找到与冲突），3 认证失败或机密已锁定，130 被中断。",
);

// ---- (2) help texts (clap) ----
// The sources are the doc comments of `cli.rs` as clap assembles them:
// consecutive `///` lines are joined with one space; lookups normalize
// whitespace, so the entries may be written on one line.
pub(crate) static HELP: &[(&str, &str)] = &[
    // Root command. The derive's bare `about` lets clap fall back to
    // `CARGO_PKG_DESCRIPTION`, which is the visible about text.
    (
        "bb, the BucketBridge command-line S3 client",
        "bb，BucketBridge 命令行 S3 客户端",
    ),
    // GlobalArgs.
    (
        "Account to use, by label or ID (`environment` is the account built from the BB_* variables).",
        "要使用的账户，按标签或 ID（`environment` 是由 BB_* 环境变量构成的账户）。",
    ),
    (
        "Use a profile of the AWS config and credentials files (honours AWS_CONFIG_FILE and AWS_SHARED_CREDENTIALS_FILE) without storing it. For `bb account import-aws`, the profiles to import (repeatable).",
        "使用 AWS config 与 credentials 文件中的配置（遵循 AWS_CONFIG_FILE 和 AWS_SHARED_CREDENTIALS_FILE），但不存储它。对 `bb account import-aws` 则是要导入的配置（可重复）。",
    ),
    (
        "Current code of the account's MFA device; logs in before the command runs.",
        "账户 MFA 设备的当前验证码；在命令运行前登录。",
    ),
    (
        "Print one JSON record per item (NDJSON); errors become JSON records on stderr.",
        "每个条目输出一行 JSON 记录（NDJSON）；错误在标准错误上变成 JSON 记录。",
    ),
    (
        "Print nothing for successful changes and no progress; failures are still reported.",
        "成功的更改不输出任何内容，也不显示进度；失败仍会报告。",
    ),
    (
        "Directory holding config.toml (overrides BB_CONFIG_DIR).",
        "存放 config.toml 的目录（覆盖 BB_CONFIG_DIR）。",
    ),
    // Command enum.
    (
        "List buckets, or the folders and objects under an S3 prefix.",
        "列出存储桶，或 S3 前缀下的文件夹与对象。",
    ),
    (
        "Add up the size and number of objects under a bucket or prefix.",
        "统计存储桶或前缀下对象的总大小和数量。",
    ),
    (
        "Walk all versions: also add up non-current versions and count delete markers.",
        "遍历所有版本：同时统计非当前版本的大小并计算删除标记数量。",
    ),
    (
        "Also add up the parts of unfinished multipart uploads.",
        "同时统计未完成分段上传的分段大小。",
    ),
    (
        "Enter the secret access key of an account that has none (an imported account). Like `add`, it is read from standard input (--secret-stdin), from BB_SECRET_ACCESS_KEY, or asked on the terminal.",
        "为没有密钥的账户（例如导入的账户）输入 Secret Access Key。与 `add` 一样，它从标准输入（--secret-stdin）、BB_SECRET_ACCESS_KEY 读取，或在终端中询问。",
    ),
    (
        "Put an account in a folder, take it out of its folder, or move it in the list.",
        "把账户放入文件夹、移出文件夹，或在列表中移动。",
    ),
    (
        "List, add, rename and remove account folders.",
        "列出、添加、重命名和删除账户文件夹。",
    ),
    (
        "Write accounts to a JSON file, without any secret (secret keys, session tokens and passwords are never written).",
        "把账户写入 JSON 文件，不含任何机密（密钥、会话令牌和密码都不会写入）。",
    ),
    (
        "Add the accounts of a file written by `bb account export`. Existing accounts are never changed; imported accounts with keys need their secret access key before they can be used.",
        "添加由 `bb account export` 写出的文件中的账户。现有账户不会被更改；带密钥的导入账户在使用前需要输入其 Secret Access Key。",
    ),
    ("Label or ID of the account.", "账户的标签或 ID。"),
    (
        "Label or ID of the account to move.",
        "要移动的账户的标签或 ID。",
    ),
    (
        "Put the account in this folder (created when it does not exist).",
        "把账户放入此文件夹（不存在时会创建）。",
    ),
    (
        "Take the account out of its folder.",
        "把账户移出其文件夹。",
    ),
    (
        "Place the account before this account (label or ID) in the list.",
        "把账户放在列表中此账户（标签或 ID）之前。",
    ),
    (
        "Place the account first in the list.",
        "把账户放在列表最前面。",
    ),
    (
        "List the folders and the accounts in them.",
        "列出文件夹及其中的账户。",
    ),
    ("Create an empty folder.", "创建一个空文件夹。"),
    ("Rename a folder.", "重命名文件夹。"),
    (
        "Remove a folder; its accounts are kept and are no longer in a folder.",
        "删除文件夹；其中的账户会保留，不再属于任何文件夹。",
    ),
    ("Folder name.", "文件夹名称。"),
    ("Current folder name.", "当前文件夹名称。"),
    ("New folder name.", "新文件夹名称。"),
    (
        "The file to write (replaced if it exists).",
        "要写入的文件（已存在则替换）。",
    ),
    (
        "Only these accounts (label or ID); default: every account. An account that assumes a role brings its source account along.",
        "仅这些账户（标签或 ID）；默认：所有账户。承担角色的账户会带上其源账户。",
    ),
    (
        "The file written by `bb account export`.",
        "由 `bb account export` 写出的文件。",
    ),
    (
        "Import an account whose label is taken under a free label (`Label (2)`) instead of leaving it out.",
        "标签已被占用的账户，改用空闲标签（`Label (2)`）导入，而不是跳过。",
    ),
    ("Make a bucket.", "创建存储桶。"),
    ("Remove an empty bucket.", "删除空存储桶。"),
    (
        "Delete objects, folders or object versions.",
        "删除对象、文件夹或对象版本。",
    ),
    (
        "Make an older version of an object its current version (a server-side copy).",
        "把对象的某个旧版本设为当前版本（服务器端复制）。",
    ),
    (
        "Copy files and objects: local to S3, S3 to local, or S3 to S3.",
        "复制文件与对象：本地到 S3、S3 到本地，或 S3 到 S3。",
    ),
    (
        "Move files and objects (each source is deleted after its copy succeeded).",
        "移动文件与对象（每个源在其复制成功后删除）。",
    ),
    (
        "Make a local folder and an S3 prefix match (upload, download or both ways).",
        "让本地文件夹与 S3 前缀一致（上传、下载或双向）。",
    ),
    (
        "Create a presigned URL for downloading or uploading one object.",
        "为一个对象的下载或上传创建预签名 URL。",
    ),
    (
        "List and abort uncompleted multipart uploads.",
        "列出并中止未完成的分片上传。",
    ),
    (
        "Security scan: public ACLs and policies, public access block, encryption, versioning, object ownership.",
        "安全扫描：公共 ACL 与策略、公共访问阻止、加密、版本控制、对象所有权。",
    ),
    (
        "Mount a bucket read-only as a folder or drive (FUSE, Dokan on Windows; needs a build with the mount feature).",
        "把存储桶以只读方式挂载为文件夹或驱动器（FUSE，Windows 上为 Dokan；需要带 mount 特性的构建）。",
    ),
    (
        "Write the listing of an S3 prefix to a CSV, Parquet or Excel (XLSX) file.",
        "把 S3 前缀的列表写入 CSV、Parquet 或 Excel（XLSX）文件。",
    ),
    (
        "Manage stored accounts (secrets live in the OS keychain).",
        "管理已存储的账户（机密保存在系统密钥链中）。",
    ),
    (
        "Read, set and delete object tags.",
        "读取、设置和删除对象标签。",
    ),
    (
        "Change the storage class of objects (a server-side copy onto themselves).",
        "更改对象的存储类型（对自身做服务器端复制）。",
    ),
    (
        "Change the server-side encryption of objects (a server-side copy onto themselves).",
        "更改对象的服务器端加密（对自身做服务器端复制）。",
    ),
    ("Read and set the ACL of objects.", "读取与设置对象的 ACL。"),
    (
        "Read and set the Object Lock retention and legal hold of objects.",
        "读取与设置对象的 Object Lock 保留与法律保留。",
    ),
    (
        "Ask S3 to restore archived (Glacier) objects.",
        "请求 S3 恢复已归档（Glacier）的对象。",
    ),
    (
        "Manage CloudFront distributions and invalidations (AWS accounts only).",
        "管理 CloudFront 分配（CloudFront）与失效（仅限 AWS 账户）。",
    ),
    (
        "Manage IAM users, access keys, policies and console passwords (AWS accounts only).",
        "管理 IAM 用户、访问密钥、策略与控制台密码（仅限 AWS 账户）。",
    ),
    (
        "Encrypt objects in place with a password (client-side encryption).",
        "用密码原位加密对象（客户端加密）。",
    ),
    (
        "Decrypt client-side encrypted objects in place.",
        "原位解密客户端加密的对象。",
    ),
    (
        "Edit an object in place: open a local copy and upload every save until Ctrl-C.",
        "原位编辑对象：打开本地副本，每次保存都上传，直到按 Ctrl-C。",
    ),
    // IamCommand and friends.
    (
        "List the IAM users of the account.",
        "列出账户的 IAM 用户。",
    ),
    (
        "Show, create, update and delete one user.",
        "查看、创建、更新和删除一个用户。",
    ),
    (
        "List, create, enable, disable and delete the access keys of a user.",
        "列出、创建、启用、停用和删除某用户的访问密钥。",
    ),
    (
        "List managed policies and attach or detach them.",
        "列出托管策略并附加或分离它们。",
    ),
    (
        "List, read, write and delete the inline policies of a user.",
        "列出、读取、写入和删除某用户的内联策略。",
    ),
    (
        "Show, create, update and delete the console password of a user.",
        "查看、创建、更新和删除某用户的控制台密码。",
    ),
    (
        "Check a user policy document (a file, or `-` for standard input) without an account.",
        "不使用账户检查用户策略文档（一个文件，或用 `-` 表示标准输入）。",
    ),
    (
        "Pretty-print a policy document (a file, or `-` for standard input) without an account.",
        "不使用账户美化打印策略文档（一个文件，或用 `-` 表示标准输入）。",
    ),
    (
        "Only users whose path starts with this prefix.",
        "只列出路径以该前缀开头的用户。",
    ),
    ("The IAM user name.", "IAM 用户名。"),
    (
        "Show a user with its policies, access keys, console password and groups.",
        "查看一个用户及其策略、访问密钥、控制台密码和组。",
    ),
    ("Create a user.", "创建一个用户。"),
    (
        "Delete a user and everything it owns (keys, policies, console password, groups); needs --yes.",
        "删除一个用户及其所有内容（密钥、策略、控制台密码、组）；需要 --yes。",
    ),
    (
        "Rename a user and/or change its path.",
        "重命名一个用户，及/或更改其路径。",
    ),
    (
        "The path of the user (default `/`).",
        "用户的路径（默认 `/`）。",
    ),
    ("Confirm the deletion.", "确认删除。"),
    ("Rename the user.", "重命名该用户。"),
    ("Move the user to another path.", "把该用户移到另一个路径。"),
    (
        "List the access keys of a user (IAM never shows their secrets).",
        "列出某用户的访问密钥（IAM 不会显示其秘密）。",
    ),
    (
        "Create an access key. The secret is printed once and cannot be read again.",
        "创建一个访问密钥。秘密访问密钥只打印一次，无法再次查看。",
    ),
    (
        "Delete an access key; needs --yes.",
        "删除一个访问密钥；需要 --yes。",
    ),
    ("Make an access key usable.", "让一个访问密钥变为可用。"),
    (
        "Keep an access key but refuse it.",
        "保留访问密钥但拒绝使用它。",
    ),
    ("The access key ID.", "访问密钥 ID。"),
    (
        "List the managed policies that can be attached.",
        "列出可附加的托管策略。",
    ),
    (
        "List the managed policies attached to a user.",
        "列出已附加到某用户的托管策略。",
    ),
    (
        "Attach a managed policy to a user.",
        "把托管策略附加到某用户。",
    ),
    (
        "Detach a managed policy from a user.",
        "把托管策略从某用户分离。",
    ),
    ("Which policies to list.", "列出哪些策略。"),
    ("The ARN of the managed policy.", "托管策略的 ARN。"),
    (
        "List the names of the inline policies of a user.",
        "列出某用户内联策略的名称。",
    ),
    ("Print an inline policy document.", "打印一个内联策略文档。"),
    (
        "Create or replace an inline policy from a file (`-` is standard input).",
        "从文件创建或替换一个内联策略（`-` 表示标准输入）。",
    ),
    (
        "Delete an inline policy; needs --yes.",
        "删除一个内联策略；需要 --yes。",
    ),
    ("The policy name.", "策略名称。"),
    (
        "The policy document (`-` reads standard input).",
        "策略文档（`-` 读取标准输入）。",
    ),
    (
        "Show whether a user has a console password.",
        "显示某用户是否有控制台密码。",
    ),
    (
        "Give a user a console password (from standard input, generated, or asked on the terminal).",
        "为某用户设置控制台密码（来自标准输入、自动生成，或在终端询问）。",
    ),
    (
        "Change the console password of a user and/or whether it must be changed at the next sign-in.",
        "更改某用户的控制台密码，及/或是否必须在下次登录时修改。",
    ),
    (
        "Remove the console password of a user; needs --yes.",
        "移除某用户的控制台密码；需要 --yes。",
    ),
    (
        "Read the password from the first line of standard input.",
        "从标准输入的第一行读取密码。",
    ),
    (
        "Generate a password and print it once.",
        "生成一个密码并只打印一次。",
    ),
    (
        "The user must choose a new password at the next sign-in.",
        "用户必须在下次登录时选择新密码。",
    ),
    (
        "Set a new password read from the first line of standard input.",
        "设置新密码，从标准输入的第一行读取。",
    ),
    (
        "Set a generated password and print it once.",
        "设置自动生成的密码并只打印一次。",
    ),
    (
        "The user need not choose a new password at the next sign-in.",
        "用户在下次登录时无需选择新密码。",
    ),
    (
        "The policy document; `-` reads standard input.",
        "策略文档；`-` 读取标准输入。",
    ),
    // Ls / Mb / Rb / Rm / RestoreVersion.
    (
        "`s3://bucket` or `s3://bucket/prefix`; omit to list buckets.",
        "`s3://bucket` 或 `s3://bucket/prefix`；省略则列出存储桶。",
    ),
    (
        "`s3://bucket` or `s3://bucket/prefix`.",
        "`s3://bucket` 或 `s3://bucket/prefix`。",
    ),
    (
        "List every object under the prefix instead of one folder level.",
        "列出前缀下的所有对象，而不只一层文件夹。",
    ),
    (
        "List object versions and delete markers (always in the long format).",
        "列出对象版本和删除标记（始终用长格式）。",
    ),
    (
        "Show modification time, size and storage class (creation date and region for buckets).",
        "显示修改时间、大小与存储类型（存储桶则显示创建日期与区域）。",
    ),
    (
        "Bucket name, or `s3://bucket`.",
        "存储桶名称或 `s3://bucket`。",
    ),
    (
        "Region to create the bucket in (default: the account region).",
        "创建存储桶的区域（默认为账户区域）。",
    ),
    (
        "Bucket name, or `s3://bucket`. The bucket must be empty.",
        "存储桶名称或 `s3://bucket`。存储桶必须为空。",
    ),
    (
        "`s3://bucket/key`, or a prefix ending in `/` (or a whole bucket) with --recursive.",
        "`s3://bucket/key`，或用 --recursive 时以 `/` 结尾的前缀（或整个存储桶）。",
    ),
    (
        "Delete everything under the prefix.",
        "删除前缀下的所有内容。",
    ),
    (
        "Delete every version and delete marker, not just the current version.",
        "删除全部版本与删除标记，而不只是当前版本。",
    ),
    (
        "Delete this one version or delete marker of the object for good.",
        "永久删除该对象的这一个版本或删除标记。",
    ),
    (
        "Print what would be deleted without deleting anything.",
        "打印（试运行）将删除的内容，但不做删除。",
    ),
    (
        "`s3://bucket/key` of the object.",
        "对象的 `s3://bucket/key`。",
    ),
    (
        "The version to make current (the IDs are in `bb ls --versions`).",
        "要设为当前版本的 ID（ID 见 `bb ls --versions`）。",
    ),
    // Copy / Cse / Encrypt / Decrypt.
    (
        "Source: a local file or folder, `s3://bucket/key`, or a prefix ending in `/`.",
        "源：本地文件或文件夹、`s3://bucket/key`，或以 `/` 结尾的前缀。",
    ),
    (
        "Destination: a local path or folder, an exact key, or a prefix (ending in `/`, or a bucket).",
        "目标：本地路径或文件夹、精确键，或前缀（以 `/` 结尾，或是一个存储桶）。",
    ),
    (
        "Copy a whole folder or prefix; the selected folder lands under the destination.",
        "复制整个文件夹或前缀；选中的文件夹会放到目标之下。",
    ),
    (
        "Include paths matching GLOB again (after an --exclude); later rules win.",
        "重新包含匹配 GLOB 的路径（放在 --exclude 之后）；后面的规则生效。",
    ),
    (
        "Skip paths matching GLOB (relative to the source folder or prefix); later rules win.",
        "跳过匹配 GLOB 的路径（相对于源文件夹或前缀）；后面的规则生效。",
    ),
    (
        "Print what would be copied without changing anything.",
        "打印将复制的内容，但不做任何更改。",
    ),
    (
        "What to do when the destination exists: overwrite, skip, newer (replace only when the source is newer) or rename (`name (1).ext`). Without it the transfer setting applies; `ask` asks for each conflict on a terminal and is refused elsewhere.",
        "目标已存在时的处理：overwrite（覆盖）、skip（跳过）、newer（源较新时才替换）或 rename（重命名，`name (1).ext`）。不带它时使用传输设置；`ask` 会在终端逐个询问冲突，其他环境会拒绝。",
    ),
    (
        "Do not store the local modification time of uploads, nor give downloaded files the time of the object.",
        "不为上传保存本地修改时间，也不让下载的文件使用对象的时间。",
    ),
    (
        "Account that owns the source (a label or ID; default: --account).",
        "拥有源的账户（标签或 ID；默认 --account）。",
    ),
    (
        "Account that owns the destination bucket, for a copy or move between S3 locations.",
        "拥有目标存储桶的账户，用于 S3 之间的复制或移动。",
    ),
    (
        "Client-side encryption of uploads and decryption of downloads.",
        "上传加密与下载解密的客户端加密选项。",
    ),
    (
        "Encrypt uploaded files with a password before they leave this machine.",
        "在文件离开本机前用密码加密上传的文件。",
    ),
    (
        "Decrypt downloaded objects that are client-side encrypted (others are saved as they are).",
        "解密下载的已客户端加密的对象（其他对象按原样保存）。",
    ),
    (
        "Compress before encrypting: none, gzip or zstd.",
        "加密前压缩：none、gzip 或 zstd。",
    ),
    (
        "Read the password from the first line of FILE (else BB_CSE_PASSWORD, else asked on the terminal).",
        "从 FILE 的第一行读取密码（否则用 BB_CSE_PASSWORD，再否则在终端询问）。",
    ),
    (
        "`s3://bucket/key`, or a prefix ending in `/` with --recursive.",
        "`s3://bucket/key`，或用 --recursive 时以 `/` 结尾的前缀。",
    ),
    (
        "Encrypt every object under the prefix (objects that are encrypted already are skipped).",
        "加密前缀下的每个对象（已加密的对象会跳过）。",
    ),
    (
        "Decrypt every object under the prefix (objects that are not encrypted are skipped).",
        "解密前缀下的每个对象（未加密的对象会跳过）。",
    ),
    // Sync.
    ("Saved sync jobs.", "已保存的同步任务。"),
    (
        "Local folder, or an `s3://` location to sync from one S3 prefix to another. Always the first argument.",
        "本地文件夹，或用于从一个 S3 前缀同步到另一个的 `s3://` 位置。始终是第一个参数。",
    ),
    (
        "`s3://bucket` or `s3://bucket/prefix`, which names a folder. Always the second argument.",
        "`s3://bucket` 或 `s3://bucket/prefix`，其指向一个文件夹。始终是第二个参数。",
    ),
    (
        "Which way files are copied (default: upload; an S3 to S3 sync always copies from the first location to the second).",
        "文件往哪个方向复制（默认 upload；S3 到 S3 的同步总是从第一个位置复制到第二个）。",
    ),
    (
        "Also delete what exists only on the target side (not with --direction bidirectional).",
        "同时删除只存在于目标侧的内容（--direction bidirectional 时不可用）。",
    ),
    (
        "Print the plan without changing anything: no file, object or stored plan is written.",
        "打印计划但不做任何更改：不写入任何文件、对象或存储的计划。",
    ),
    (
        "Finish the interrupted plan with this ID; the folders, direction and --delete come from the plan.",
        "用这个 ID 续跑被中断的计划；文件夹、方向和 --delete 都来自该计划。",
    ),
    (
        "Run the saved sync job with this name (or ID); it uses the job's account.",
        "以这个名字（或 ID）运行已保存的同步任务；它使用任务所属的账户。",
    ),
    (
        "Save this sync as a job with this name (replacing a job of that name), then run it.",
        "把这个同步保存为以该名字命名的任务（覆盖同名任务），然后运行它。",
    ),
    (
        "Gitignore-style rule that excludes paths (repeatable; `!` negates, a trailing `/` matches folders).",
        "排除路径的 gitignore 风格规则（可重复；`!` 表示取反，结尾的 `/` 匹配文件夹）。",
    ),
    (
        "Sync only paths that match this glob (repeatable).",
        "只同步匹配此 glob 的路径（可重复）。",
    ),
    (
        "Read exclusion rules from a file, one gitignore-style rule per line.",
        "从文件读取排除规则，每行一条 gitignore 风格的规则。",
    ),
    (
        "Also sync empty folders (empty local folders get a folder marker in S3, and the other way round).",
        "也同步空文件夹（空本地文件夹会在 S3 得到文件夹标记，反之亦然）。",
    ),
    (
        "Treat files as equal when their size and checksum match, even if the modification times differ.",
        "当大小与校验和相同就视为文件相等，即使修改时间不同。",
    ),
    (
        "Do not use the local cache of remote listings.",
        "不使用远端列表的本地缓存。",
    ),
    (
        "How many remote folders are listed at the same time (1 to 32).",
        "同时列出多少个远端文件夹（1 到 32）。",
    ),
    (
        "Print a report at the end: counts and bytes per action, the duration and the failures (with --json, a final `report` record).",
        "结束时打印报告：各操作的次数与字节数、耗时和失败项（--json 时最后输出一条 `report` 记录）。",
    ),
    (
        "List, show and delete saved sync jobs (jobs are created in the desktop app or with --save-job).",
        "列出、查看和删除已保存的同步任务（任务在桌面应用中或用 --save-job 创建）。",
    ),
    ("List the saved sync jobs.", "列出已保存的同步任务。"),
    ("Show one saved sync job.", "查看一个已保存的同步任务。"),
    (
        "Delete a saved sync job (needs --yes).",
        "删除一个已保存的同步任务（需要 --yes）。",
    ),
    (
        "Name (any letter case) or ID of the job.",
        "任务的名称（大小写不限）或 ID。",
    ),
    // Edit.
    (
        "Open the copy with this application (an .app bundle on macOS, a program elsewhere) instead of the default one.",
        "用该应用程序（macOS 上是 .app 包，其他平台是一个程序）打开副本，而不是默认应用。",
    ),
    // Presign.
    (
        "How long the URL is valid: 30s, 15m, 1h, 7d, 1h30m or seconds (1s to 7d).",
        "URL 的有效时长：30s、15m、1h、7d、1h30m 或秒数（1s 到 7d）。",
    ),
    (
        "Whether the URL downloads (get) or uploads (put) the object.",
        "这个 URL 是下载（get）还是上传（put）对象。",
    ),
    (
        "Also send the URL to the URL shortener chosen in the settings and print the short link. The whole URL, signature included, leaves this machine.",
        "同时把 URL 发送到设置中选择的短链服务并打印短链接。完整 URL（包括签名）会离开本机。",
    ),
    // Export.
    (
        "`s3://bucket` or `s3://bucket/prefix`; the prefix is listed like `bb ls` does.",
        "`s3://bucket` 或 `s3://bucket/prefix`；前缀按 `bb ls` 的方式列出。",
    ),
    ("File format.", "文件格式。"),
    (
        "File to write; it is replaced when the export is complete.",
        "要写入的文件；导出完成后替换它。",
    ),
    (
        "Every object below the prefix instead of one folder level.",
        "前缀之下的所有对象，而不只一层文件夹。",
    ),
    // Mpu.
    (
        "List uncompleted multipart uploads.",
        "列出未完成的分片上传。",
    ),
    (
        "Abort uncompleted multipart uploads (frees the storage of their parts).",
        "中止未完成的分片上传（释放分片占用的存储）。",
    ),
    ("Bucket to mount.", "要挂载的存储桶。"),
    (
        "Existing empty folder to mount it on (Windows: a free drive letter such as X: also works).",
        "挂载到的已有空文件夹（Windows 上也可以用空闲的盘符，如 X:）。",
    ),
    (
        "Only this prefix of the bucket.",
        "只挂载存储桶的这一前缀。",
    ),
    // Scan / Mpu list/abort.
    (
        "Only this bucket (default: every bucket of the account).",
        "只扫描该存储桶（默认：账户的所有存储桶）。",
    ),
    (
        "Only this bucket (default: every bucket).",
        "只查看该存储桶（默认：所有存储桶）。",
    ),
    (
        "Only uploads whose key starts with PREFIX.",
        "只列出键以 PREFIX 开头的上传。",
    ),
    ("Bucket of the uploads.", "这些上传所属的存储桶。"),
    (
        "Key of the upload to abort (with --upload-id).",
        "要中止的上传的键（与 --upload-id 一起用）。",
    ),
    (
        "ID of the upload to abort (with --key).",
        "要中止的上传的 ID（与 --key 一起用）。",
    ),
    (
        "Abort every uncompleted upload in the bucket (or under --prefix).",
        "中止存储桶中（或 --prefix 之下）所有未完成的上传。",
    ),
    (
        "With --all: only uploads whose key starts with PREFIX.",
        "配合 --all：只中止键以 PREFIX 开头的上传。",
    ),
    (
        "Print what would be aborted without aborting anything.",
        "打印将中止的内容，但不做中止。",
    ),
    // Account.
    (
        "Store a new account. The secret access key is never a command-line argument: it is read from standard input (--secret-stdin), from BB_SECRET_ACCESS_KEY, or asked on the terminal.",
        "存储一个新账户。秘密访问密钥绝不作为命令行参数：从标准输入（--secret-stdin）、BB_SECRET_ACCESS_KEY 读取，或在终端询问。",
    ),
    (
        "List the stored accounts (`*` marks the default).",
        "列出已存储的账户（`*` 是默认账户）。",
    ),
    (
        "Remove a stored account and its secrets.",
        "移除一个已存储账户及其机密。",
    ),
    (
        "Make a stored account the default.",
        "把一个已存储账户设为默认。",
    ),
    (
        "Import profiles of the AWS config and credentials files as accounts that stay linked to the files (no key is copied). Name profiles with --profile; without it every usable profile is imported.",
        "把 AWS config 与 credentials 文件中的配置导入为保持与文件关联的账户（不复制任何密钥）。用 --profile 指定配置；不带它则导入所有可用的配置。",
    ),
    (
        "Sign in to AWS IAM Identity Center (SSO) with the device authorization flow; the token goes to the AWS CLI cache.",
        "通过设备授权流程登录 AWS IAM Identity Center（SSO）；令牌存入 AWS CLI 缓存。",
    ),
    (
        "Label or ID of a stored SSO account whose portal to sign in to.",
        "要登录其门户的已存储 SSO 账户的标签或 ID。",
    ),
    (
        "Start URL of the portal (https://<directory>.awsapps.com/start).",
        "门户的起始 URL（https://<directory>.awsapps.com/start）。",
    ),
    (
        "Region IAM Identity Center runs in.",
        "IAM Identity Center 所在的区域。",
    ),
    (
        "Name of the sso-session the token is cached under (as in the AWS config).",
        "令牌缓存所在的 sso-session 名称（与 AWS config 中一致）。",
    ),
    (
        "Only print the URL and code; do not open the browser.",
        "只打印 URL 和验证码；不打开浏览器。",
    ),
    (
        "Display label, unique across accounts (case-insensitive).",
        "显示标签，在所有账户中唯一（不区分大小写）。",
    ),
    (
        "Access key ID (not a secret). Not used with --assume-role, --sso or --instance-role.",
        "访问密钥 ID（不是机密）。不与 --assume-role、--sso 或 --instance-role 一起使用。",
    ),
    (
        "Use the role of this EC2 instance or ECS/EKS container instead of keys.",
        "使用这台 EC2 实例或 ECS/EKS 容器的角色，而不是密钥。",
    ),
    (
        "With --instance-role: add the account without checking that the role exists.",
        "配合 --instance-role：添加账户时不检查角色是否存在。",
    ),
    (
        "ARN of a role to assume through --source-account.",
        "要通过 --source-account 扮演的角色的 ARN。",
    ),
    (
        "Label or ID of the stored account that signs the AssumeRole call.",
        "为 AssumeRole 调用签名的已存储账户的标签或 ID。",
    ),
    ("Role session name.", "角色会话名称。"),
    (
        "Role session length (15m to 12h; seconds or 30m, 1h, ...).",
        "角色会话时长（15m 到 12h；秒数或 30m、1h 等）。",
    ),
    (
        "External id the role's trust policy asks for.",
        "角色信任策略要求的 external id。",
    ),
    (
        "Use an AWS IAM Identity Center (SSO) role (needs the --sso-* options).",
        "使用 AWS IAM Identity Center（SSO）角色（需要 --sso-* 选项）。",
    ),
    ("SSO portal start URL.", "SSO 门户的起始 URL。"),
    ("SSO region.", "SSO 区域。"),
    (
        "AWS account id of the SSO role.",
        "SSO 角色的 AWS 账户 ID。",
    ),
    (
        "Permission set (role) name of the SSO role.",
        "SSO 角色的权限集（角色）名称。",
    ),
    (
        "Name of the sso-session the token is cached under.",
        "令牌缓存所在的 sso-session 名称。",
    ),
    ("Signing region.", "签名区域。"),
    (
        "Custom endpoint URL for S3-compatible providers.",
        "S3 兼容服务商的自定义端点 URL。",
    ),
    ("Use path-style addressing.", "使用路径风格寻址。"),
    (
        "Pay for requests to requester-pays buckets.",
        "为请求者付费（requester-pays）存储桶的请求付费。",
    ),
    (
        "Serial number or ARN of the account's MFA device (for --assume-role: the device the role's trust policy asks for).",
        "账户 MFA 设备的序列号或 ARN（对 --assume-role：角色信任策略要求的设备）。",
    ),
    (
        "Read the secret access key from the first line of standard input (a second line, if present, is a session token).",
        "从标准输入第一行读取秘密访问密钥（如有第二行，则是会话令牌）。",
    ),
    (
        "Label or ID of the account. (Not called `account`: that is the ID of the global `--account`, and clap shares IDs with subcommands.)",
        "账户的标签或 ID。（不叫 `account`：那是全局 `--account` 的 ID，而 clap 会在子命令之间共享 ID。）",
    ),
    // Tag / StorageClass / Sse / Acl.
    (
        "Print the tags of an object, or of every object under a prefix.",
        "打印一个对象或一个前缀下所有对象的标签。",
    ),
    (
        "Add tags to objects; with --replace make them the whole tag set.",
        "给对象添加标签；用 --replace 使其成为完整标签集。",
    ),
    (
        "Remove the named tags from objects, or every tag when none is named.",
        "移除对象上指定的标签；未指定时移除全部标签。",
    ),
    (
        "Read the tags of every object under the prefix.",
        "读取前缀下每个对象的标签。",
    ),
    (
        "The tags to set, each `KEY=VALUE`. Existing tags with other keys stay.",
        "要设置的标签，每个形如 `KEY=VALUE`。其他键的已有标签保留。",
    ),
    (
        "Make these tags the whole tag set, removing every other tag.",
        "使这些标签成为完整标签集，移除所有其他标签。",
    ),
    (
        "Tag every object under the prefix.",
        "给前缀下的每个对象加标签。",
    ),
    (
        "Print what would change without changing anything.",
        "打印将发生的更改，但不做更改。",
    ),
    (
        "Keys of the tags to remove; without any, every tag is removed.",
        "要移除的标签键；不给时移除所有标签。",
    ),
    (
        "Remove tags from every object under the prefix.",
        "从前缀下的每个对象移除标签。",
    ),
    (
        "STANDARD, STANDARD_IA, ONEZONE_IA, GLACIER_IR, GLACIER, DEEP_ARCHIVE, INTELLIGENT_TIERING or REDUCED_REDUNDANCY.",
        "STANDARD、STANDARD_IA、ONEZONE_IA、GLACIER_IR、GLACIER、DEEP_ARCHIVE、INTELLIGENT_TIERING 或 REDUCED_REDUNDANCY。",
    ),
    (
        "Change every object under the prefix.",
        "更改前缀下的每个对象。",
    ),
    (
        "File with the SSE-C key of objects that are encrypted with a customer key (32 raw bytes, 64 hex digits or base64).",
        "存放对象所用客户密钥（SSE-C）的文件（32 个原始字节、64 位十六进制或 base64）。",
    ),
    (
        "`s3` (SSE-S3), `kms` or `kms:KEYID` (SSE-KMS; KEYID is a key ID, ARN or alias, without it the AWS managed key) or `c` (SSE-C, needs --key-file).",
        "`s3`（SSE-S3）、`kms` 或 `kms:KEYID`（SSE-KMS；KEYID 是密钥 ID、ARN 或别名，不带时使用 AWS 托管密钥），或 `c`（SSE-C，需要 --key-file）。",
    ),
    (
        "File with the new SSE-C key (32 raw bytes, 64 hex digits or base64).",
        "存放新 SSE-C 密钥的文件（32 个原始字节、64 位十六进制或 base64）。",
    ),
    (
        "File with the SSE-C key the objects have now, for objects that are encrypted with a customer key.",
        "存放对象当前所用 SSE-C 密钥的文件，用于以客户密钥加密的对象。",
    ),
    (
        "With kms: use an S3 Bucket Key.",
        "配合 kms：使用 S3 存储桶密钥（S3 Bucket Key）。",
    ),
    (
        "Apply a canned ACL to objects.",
        "为对象应用一个预定义 ACL（canned ACL）。",
    ),
    (
        "Print the ACL of an object, or of every object under a prefix.",
        "打印一个对象或一个前缀下所有对象的 ACL。",
    ),
    (
        "Read the ACL of every object under the prefix.",
        "读取前缀下每个对象的 ACL。",
    ),
    (
        "private, public-read, public-read-write, authenticated-read, bucket-owner-read or bucket-owner-full-control.",
        "private、public-read、public-read-write、authenticated-read、bucket-owner-read 或 bucket-owner-full-control。",
    ),
    (
        "Apply it to every object under the prefix.",
        "应用于前缀下的每个对象。",
    ),
    // Lock / Restore.
    (
        "Print the retention and legal hold of an object, or of every object under a prefix.",
        "打印一个对象或一个前缀下所有对象的保留与法律保留。",
    ),
    (
        "Set or clear the retention and the legal hold of objects.",
        "设置或清除对象的保留与法律保留。",
    ),
    (
        "Read every object under the prefix.",
        "读取前缀下的每个对象。",
    ),
    ("Read this version of the object.", "读取该对象的这个版本。"),
    (
        "Retention mode: GOVERNANCE or COMPLIANCE (needs --until).",
        "保留模式：GOVERNANCE 或 COMPLIANCE（需要 --until）。",
    ),
    (
        "End of the retention: an RFC 3339 time or a date (YYYY-MM-DD, UTC midnight).",
        "保留的截止时间：RFC 3339 时间或日期（YYYY-MM-DD，UTC 零点）。",
    ),
    (
        "Remove the retention (a governance retention needs --bypass-governance).",
        "移除保留（治理型保留需要 --bypass-governance）。",
    ),
    ("Turn the legal hold on or off.", "打开或关闭法律保留。"),
    (
        "Let a user with s3:BypassGovernanceRetention shorten or remove a governance retention.",
        "允许拥有 s3:BypassGovernanceRetention 的用户缩短或移除治理型保留。",
    ),
    (
        "Change this version of the object.",
        "更改该对象的这个版本。",
    ),
    (
        "Days the restored copy stays available.",
        "恢复的副本保持可用的天数。",
    ),
    (
        "Expedited, Standard or Bulk.",
        "Expedited、Standard 或 Bulk。",
    ),
    (
        "Restore every object under the prefix.",
        "恢复前缀下的每个对象。",
    ),
    (
        "Restore this version of the object.",
        "恢复该对象的这个版本。",
    ),
    (
        "Print what would be requested without requesting anything.",
        "打印将请求的内容，但不发起请求。",
    ),
    // Cloudfront.
    (
        "List the distributions of the account, or those that serve one bucket.",
        "列出账户的分配（CloudFront），或服务某个存储桶的那些。",
    ),
    (
        "Show one distribution with its origins and aliases.",
        "查看一个分配（CloudFront）的源与别名。",
    ),
    (
        "Create a distribution for a bucket (the bucket policy to add is printed).",
        "为存储桶创建分配（CloudFront）（会打印要添加的存储桶策略）。",
    ),
    (
        "Let a distribution accept requests.",
        "让分配（CloudFront）接受请求。",
    ),
    (
        "Stop a distribution from accepting requests.",
        "让分配（CloudFront）停止接受请求。",
    ),
    (
        "Stop a distribution from serving a bucket (its origins are removed; nothing is deleted).",
        "让分配（CloudFront）停止服务该存储桶（会移除其源；不删除任何内容）。",
    ),
    (
        "Invalidate cached paths (each starts with `/`; `*` only as the last character).",
        "失效缓存的路径（每条以 `/` 开头；`*` 只能作为最后一个字符）。",
    ),
    (
        "List the invalidation history of a distribution, newest first.",
        "列出分配（CloudFront）的失效历史，最新的在前。",
    ),
    (
        "Show one invalidation with its paths.",
        "查看一个失效及其路径。",
    ),
    (
        "Only the distributions that have an origin in this bucket.",
        "只列出有源在该存储桶中的分配（CloudFront）。",
    ),
    (
        "Distribution ID, such as `E2ABCDEFGHIJKL`.",
        "分配（CloudFront）ID，例如 `E2ABCDEFGHIJKL`。",
    ),
    ("The bucket to serve.", "要服务的存储桶。"),
    (
        "Region of the bucket (default: the region `bb ls` reports for it).",
        "存储桶的区域（默认：`bb ls` 报告给它的区域）。",
    ),
    (
        "Comment of the distribution (at most 128 characters).",
        "分配（CloudFront）的备注（最多 128 个字符）。",
    ),
    (
        "Object served for the root URL, without a leading `/`.",
        "根 URL 所服务的对象，不带开头的 `/`。",
    ),
    (
        "`PriceClass_All` (default), `PriceClass_200` or `PriceClass_100`.",
        "`PriceClass_All`（默认）、`PriceClass_200` 或 `PriceClass_100`。",
    ),
    (
        "Create the distribution disabled.",
        "创建时即停用的分配（CloudFront）。",
    ),
    ("Distribution ID.", "分配（CloudFront）ID。"),
    (
        "The bucket whose origins are removed.",
        "要移除其源的存储桶。",
    ),
    (
        "Paths to invalidate, such as `/index.html` or `/images/*`.",
        "要失效的路径，例如 `/index.html` 或 `/images/*`。",
    ),
    ("Invalidation ID.", "失效 ID。"),
];

// ---- (3) user-visible output of the commands ----
// `{name}` placeholders are filled by `trf`. Values themselves (paths,
// URIs, names, server messages) are never translated. Help/output
// fragments that tests assert on their own are registered as their own
// keys (the fragment is always a substring of a real source text).
pub(crate) static OUTPUT: &[(&str, &str)] = &[
    // The skeleton clap prints around the help text.
    ("Usage:", "用法："),
    ("Commands:", "命令："),
    ("Options:", "选项："),
    ("Arguments:", "参数："),
    // Generic verbs and column words of the transfer commands.
    ("latest", "最新"),
    ("files", "个文件"),
    ("objects", "个对象"),
    ("uploads", "个上传"),
    ("versions", "个版本"),
    ("could not be uploaded", "未能上传"),
    ("could not be downloaded", "未能下载"),
    ("could not be copied", "未能复制"),
    ("could not be moved", "未能移动"),
    ("could not be aborted", "未能中止"),
    ("could not be deleted", "未能删除"),
    ("could not be read", "未能读取"),
    ("could not be changed", "未能更改"),
    ("could not be tagged", "未能加标签"),
    ("could not be restored", "未能恢复"),
    ("could not be encrypted", "未能加密"),
    ("could not be decrypted", "未能解密"),
    (
        "{failed} of {total} {noun} {failure}",
        "共 {total} {noun}，有 {failed} {noun} {failure}",
    ),
    ("listing {uri}", "列出 {uri}"),
    ("listing s3://{bucket}/", "列出 s3://{bucket}/"),
    ("listing versions of {uri}", "列出 {uri} 的版本"),
    (
        "listing versions of {uri}: the server reported more results but sent no marker",
        "列出 {uri} 的版本：服务器报告还有更多结果，但没有发送标记",
    ),
    (
        "{what}: the server returned a continuation token it had already sent; stopping to avoid an endless listing",
        "{what}：服务器返回了它已经发过的续传标记；为避免无限列出而停止",
    ),
    (
        "the server returned a continuation token it had already sent; stopping to avoid an endless listing",
        "服务器返回了它已经发过的续传标记；为避免无限列出而停止",
    ),
    ("needs --yes", "需要 --yes"),
    ("{what} needs --yes", "{what} 需要 --yes"),
    ("reading '{path}'", "读取 '{path}' 出错"),
    ("deleting '{path}'", "删除 '{path}' 出错"),
    ("opening {path}", "打开 {path} 出错"),
    (
        "bb: error: starting the command: {error}",
        "bb: error: 启动命令出错：{error}",
    ),
    (
        "bb: error: starting the async runtime: {error}",
        "bb: error: 启动异步运行时出错：{error}",
    ),
    // main.rs progress.
    // app.rs keeps the `bb: error: ` prefix outside the translation; the
    // command messages themselves come from above.
    (
        "{done} of {total} ({percent}%), {rate}/s, {items_done} of {items} done",
        "已完成 {done}/{total}（{percent}%），{rate}/s，{items_done}/{items} 项",
    ),
    ("{done} of {total}", "{done}/{total}"),
    ("{items_done} of {items} done", "{items_done}/{items} 项"),
    // output.rs notices of done items keep the `bb: ` prefix outside.

    // bucket.rs
    (
        "created bucket {bucket} in {region}",
        "已创建存储桶 {bucket}（{region}）",
    ),
    ("created bucket {bucket}", "已创建存储桶 {bucket}"),
    ("removed bucket {bucket}", "已删除存储桶 {bucket}"),
    // ls.rs stays data-only; version rows keep PRE/DEL tokens.

    // rm.rs
    ("deleted s3://{bucket}/{key}", "已删除 s3://{bucket}/{key}"),
    (
        "would delete s3://{bucket}/{key}",
        "（试运行）将删除 s3://{bucket}/{key}",
    ),
    ("would delete {source}", "（试运行）将删除 {source}"),
    ("would ", "（试运行）将"),
    (
        "deleted s3://{bucket}/{key} (version {version})",
        "已删除 s3://{bucket}/{key}（版本 {version}）",
    ),
    (
        "would delete s3://{bucket}/{key} (version {version})",
        "（试运行）将删除 s3://{bucket}/{key}（版本 {version}）",
    ),
    (
        "failed to delete s3://{bucket}/{key}: {message}",
        "无法删除 s3://{bucket}/{key}：{message}",
    ),
    (
        "failed to delete s3://{bucket}/{key} (version {version}): {error}",
        "无法删除 s3://{bucket}/{key}（版本 {version}）：{error}",
    ),
    ("{uri} has no versions", "{uri} 没有任何版本"),
    (
        "--version-id needs an object key; {uri} is a prefix",
        "--version-id 需要对象键；{uri} 是一个前缀",
    ),
    (
        "{uri} is a prefix; add --recursive to delete everything under it",
        "{uri} 是一个前缀；请加 --recursive 以删除其下所有内容",
    ),
    (
        "--recursive needs a prefix ending in '/', for example {uri}/",
        "--recursive 需要以 '/' 结尾的前缀，例如 {uri}/",
    ),
    ("the path is empty", "路径为空"),
    (
        "'{text}' names an object; expected a bucket such as s3://bucket",
        "'{text}' 是一个对象；应为 s3://bucket 这样的存储桶",
    ),
    ("names an object", "是一个对象"),
    (
        "unknown compression '{value}' (none, gzip or zstd)",
        "未知的压缩方式 '{value}'（none、gzip 或 zstd）",
    ),
    (
        "'{text}' is not a date; use YYYY-MM-DD or an RFC 3339 time",
        "'{text}' 不是日期；请用 YYYY-MM-DD 或 RFC 3339 时间",
    ),
    ("'{text}' is not KEY=VALUE", "'{text}' 不是 KEY=VALUE"),
    (
        "kms: needs a key ID, ARN or alias after the colon",
        "kms: 冒号后需要密钥 ID、ARN 或别名",
    ),
    (
        "'{text}' is not an encryption; use s3, kms, kms:KEYID or c",
        "'{text}' 不是加密方式；请用 s3、kms、kms:KEYID 或 c",
    ),
    (
        "'{text}' is not a plan ID (expected a UUID such as 5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35)",
        "'{text}' 不是计划 ID（应为 5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35 这样的 UUID）",
    ),
    (
        "(bb sync takes the local folder first and the S3 location second; two S3 locations sync from the first to the second)",
        "（bb sync 先取本地文件夹、后取 S3 位置；两个 S3 位置则从第一个同步到第二个）",
    ),
    // copy.rs / mv
    ("secret access key: {secret}", "秘密访问密钥：{secret}"),
    ("access key ID: {key_id}", "访问密钥 ID：{key_id}"),
    ("both locations are local", "两个位置都是本地路径"),
    (
        "both locations are local paths; bb uploads files to S3, downloads objects from S3 and copies objects between S3 locations",
        "两个位置都是本地路径；bb 把文件上传到 S3、从 S3 下载对象，并在 S3 位置之间复制对象",
    ),
    (
        "--from-account names the account of an S3 source; the source here is a local path",
        "--from-account 指定 S3 源的账户；这里的源是本地路径",
    ),
    (
        "--to-account names the account of an S3 destination; the destination here is a local path",
        "--to-account 指定 S3 目标的账户；这里的目标是本地路径",
    ),
    (
        "--recursive needs a source prefix ending in '/', for example {source}/",
        "--recursive 需要以 '/' 结尾的源前缀，例如 {source}/",
    ),
    (
        "{source} is a prefix; add --recursive to upload everything under it",
        "{source} 是一个前缀；请加 --recursive 以上传其下所有内容",
    ),
    (
        "{source} is a prefix; add --recursive to download everything under it",
        "{source} 是一个前缀；请加 --recursive 以下载其下所有内容",
    ),
    (
        "{source} is a prefix; add --recursive to copy everything under it",
        "{source} 是一个前缀；请加 --recursive 以复制其下所有内容",
    ),
    (
        "{source} is a prefix; add --recursive to move everything under it",
        "{source} 是一个前缀；请加 --recursive 以移动其下所有内容",
    ),
    (
        "cannot upload {source} onto itself",
        "无法把 {source} 上传到它自己之上",
    ),
    (
        "cannot download {source} onto itself",
        "无法把 {source} 下载到它自己之上",
    ),
    (
        "cannot copy {source} onto itself",
        "无法把 {source} 复制到它自己之上",
    ),
    (
        "cannot move {source} onto itself",
        "无法把 {source} 移动到它自己之上",
    ),
    (
        "--password-file needs --encrypt or --decrypt",
        "--password-file 需要 --encrypt 或 --decrypt",
    ),
    (
        "--decrypt applies to downloads; to decrypt an object in S3 use bb decrypt",
        "--decrypt 只适用于下载；要在 S3 中解密对象请用 bb decrypt",
    ),
    (
        "--encrypt applies to uploads; to encrypt an object in S3 use bb encrypt",
        "--encrypt 只适用于上传；要在 S3 中加密对象请用 bb encrypt",
    ),
    (
        "--encrypt and --decrypt do not apply to copies between S3 locations; use bb encrypt or bb decrypt",
        "--encrypt 与 --decrypt 不适用于 S3 位置之间的复制；请用 bb encrypt 或 bb decrypt",
    ),
    (
        "the overwrite policy is 'ask', which needs a terminal; pass --if-exists overwrite, skip, newer or rename",
        "覆盖策略是 'ask'，需要终端；请传 --if-exists overwrite、skip、newer 或 rename",
    ),
    (
        "'{path}' is already client-side encrypted; it is not encrypted a second time",
        "'{path}' 已经是客户端加密的；不会再次加密",
    ),
    (
        "'{path}' is a folder; add --recursive to upload it",
        "'{path}' 是一个文件夹；请加 --recursive 以上传它",
    ),
    (
        "'{path}' is a folder; add --recursive to download it",
        "'{path}' 是一个文件夹；请加 --recursive 以下载它",
    ),
    (
        "'{path}' is a folder; add --recursive to copy it",
        "'{path}' 是一个文件夹；请加 --recursive 以复制它",
    ),
    (
        "'{path}' is a folder; add --recursive to move it",
        "'{path}' 是一个文件夹；请加 --recursive 以移动它",
    ),
    (
        "'{path}' is a file; a recursive download needs a folder to put the objects in",
        "'{path}' 是一个文件；递归下载需要一个文件夹来存放对象",
    ),
    (
        "'{path}' is a file; a recursive move needs a folder to put the objects in",
        "'{path}' 是一个文件；递归移动需要一个文件夹来存放对象",
    ),
    (
        "'{path}' has no name that can be used in an S3 key (it must end with a valid UTF-8 name)",
        "'{path}' 没有可用作 S3 键的名称（必须以有效的 UTF-8 名称结尾）",
    ),
    (
        "cannot upload {source}: {message}{more}; nothing was uploaded",
        "无法上传 {source}：{message}{more}；没有上传任何内容",
    ),
    (
        "cannot download {source}: {message}{more}; nothing was downloaded",
        "无法下载 {source}：{message}{more}；没有下载任何内容",
    ),
    (
        "cannot copy {source}: {message}{more}; nothing was copied",
        "无法复制 {source}：{message}{more}；没有复制任何内容",
    ),
    (
        "cannot move {source}: {message}{more}; nothing was moved",
        "无法移动 {source}：{message}{more}；没有移动任何内容",
    ),
    (" (and {more} more)", "（另有 {more} 个）"),
    (
        "no download was planned for {source}",
        "没有为 {source} 计划任何下载",
    ),
    (
        "uploaded {source} to {destination}",
        "已上传 {source} 到 {destination}",
    ),
    (
        "downloaded {source} to {destination}",
        "已下载 {source} 到 {destination}",
    ),
    (
        "copied {source} to {destination}",
        "已复制 {source} 到 {destination}",
    ),
    ("copied s3://", "已复制 s3://"),
    ("deleted {source}", "已删除 {source}"),
    (
        "moved {source} to {destination}",
        "已移动 {source} 到 {destination}",
    ),
    (
        "would upload {source} to {destination}",
        "（试运行）将上传 {source} 到 {destination}",
    ),
    (
        "would download {source} to {destination}",
        "（试运行）将下载 {source} 到 {destination}",
    ),
    (
        "would copy {source} to {destination}",
        "（试运行）将复制 {source} 到 {destination}",
    ),
    (
        "would move {source} to {destination}",
        "（试运行）将移动 {source} 到 {destination}",
    ),
    (
        "skipped {source} to {destination}: {reason}",
        "已跳过 {source} 到 {destination}：{reason}",
    ),
    (
        "failed to upload {source} to {destination}: {error}",
        "无法上传 {source} 到 {destination}：{error}",
    ),
    (
        "failed to download {source} to {destination}: {error}",
        "无法下载 {source} 到 {destination}：{error}",
    ),
    (
        "failed to download {source} to {destination}: {why}",
        "无法下载 {source} 到 {destination}：{why}",
    ),
    (
        "failed to copy {source} to {destination}: {error}",
        "无法复制 {source} 到 {destination}：{error}",
    ),
    (
        "failed to copy {source} to {destination}: {why}",
        "无法复制 {source} 到 {destination}：{why}",
    ),
    (
        "failed to move {source} to {destination}: {error}",
        "无法移动 {source} 到 {destination}：{error}",
    ),
    (
        "failed to upload {source} to {destination}: {why}",
        "无法上传 {source} 到 {destination}：{why}",
    ),
    (
        "the upload succeeded but the source could not be deleted",
        "上传成功，但源未能删除",
    ),
    (
        "the download succeeded but the source could not be deleted",
        "下载成功，但源未能删除",
    ),
    (
        "the copy succeeded but the source could not be deleted",
        "复制成功，但源未能删除",
    ),
    (
        "the source {source} was kept because the copy {landed} could not be verified",
        "保留源 {source}，因为无法校验副本 {landed}",
    ),
    // sync.rs
    (
        "--direction does not apply to a sync between two S3 locations: it always copies from the first to the second",
        "--direction 不适用于两个 S3 位置之间的同步：它总是从第一个位置复制到第二个",
    ),
    (
        "--delete cannot be combined with --direction bidirectional: without a record of earlier runs, a file that is missing on one side may have been deleted there or may be new on the other side",
        "--delete 不能与 --direction bidirectional 一起使用：没有早期运行的记录时，一侧缺失的文件可能是在那一侧被删除的，也可能是另一侧新出现的",
    ),
    (
        "bb sync needs a local folder and an S3 location, two S3 locations, --job NAME or --resume PLAN_ID",
        "bb sync 需要一个本地文件夹和一个 S3 位置、两个 S3 位置、--job NAME 或 --resume PLAN_ID",
    ),
    (
        "invalid exclusion file '{file}': {error}",
        "排除规则文件 '{file}' 无效：{error}",
    ),
    (
        "invalid local folder '{folder}': {error}",
        "本地文件夹 '{folder}' 无效：{error}",
    ),
    (
        "no configuration directory for the sync jobs: set {env} or pass --config-dir",
        "同步任务没有配置目录：请设置 {env} 或传 --config-dir",
    ),
    (
        "no configuration directory for the sync plans: set {env} or pass --config-dir",
        "同步计划没有配置目录：请设置 {env} 或传 --config-dir",
    ),
    (
        "the sync job '{name}' runs with the account {account}; drop --account or use --account {account2}",
        "同步任务 '{name}' 使用账户 {account}；请去掉 --account 或使用 --account {account2}",
    ),
    ("bb: saved sync job '{name}'", "bb: 已保存同步任务 '{name}'"),
    (
        "plan {id} was made with account '{account}'; resume it with --account {account2}",
        "计划 {id} 是用账户 '{account}' 创建的；续跑请用 --account {account2}",
    ),
    (
        "bb: interrupted; stopping. Resume with: bb sync --resume {plan_id}",
        "bb: 已中断；正在停止。恢复请用：bb sync --resume {plan_id}",
    ),
    (
        "bb: {failed} failed while running; try again with: bb sync --resume {plan_id}",
        "bb: 有 {failed} 个在运行中失败；重试请用：bb sync --resume {plan_id}",
    ),
    ("unknown error", "未知错误"),
    ("cannot sync {path}", "无法同步 {path}"),
    ("cannot sync {path}: {why}", "无法同步 {path}：{why}"),
    (
        "failed to delete {source}: {why}",
        "无法删除 {source}：{why}",
    ),
    ("sync plan {plan_id}", "同步计划 {plan_id}"),
    (
        "{copied} copied, {deleted} deleted, {equal} already in sync",
        "已复制 {copied} 个，已删除 {deleted} 个，{equal} 个已处于同步状态",
    ),
    (", {failed} failed", "，{failed} 个失败"),
    (", {pending} not done", "，{pending} 个未完成"),
    (
        "would copy {copy}, delete {delete}; {equal} already in sync",
        "（试运行）将复制 {copy} 个，删除 {delete} 个；{equal} 个已处于同步状态",
    ),
    ("summary:", "摘要："),
    (
        "{failed} of {total} files could not be synced",
        "共 {total} 个文件，有 {failed} 个文件未能同步",
    ),
    // cse.rs
    (
        "--recursive needs a prefix ending in '/', for example {prefix}/",
        "--recursive 需要以 '/' 结尾的前缀，例如 {prefix}/",
    ),
    (
        "{uri} is a prefix; add --recursive to encrypt everything under it",
        "{uri} 是一个前缀；请加 --recursive 以加密其下所有内容",
    ),
    (
        "{uri} is a prefix; add --recursive to decrypt everything under it",
        "{uri} 是一个前缀；请加 --recursive 以解密其下所有内容",
    ),
    (
        "failed to encrypt {uri}: {error}",
        "无法加密 {uri}：{error}",
    ),
    (
        "failed to decrypt {uri}: {error}",
        "无法解密 {uri}：{error}",
    ),
    (
        "{prefix} is a prefix; add --recursive to {verb} everything under it",
        "{prefix} 是一个前缀；请加 --recursive 以{verb}其下所有内容",
    ),
    (
        "failed to {action} s3://{bucket}/{key}: {error}",
        "无法{action} s3://{bucket}/{key}：{error}",
    ),
    (
        "no encryption password: use --password-file, set {env}, or run on a terminal",
        "没有加密密码：请用 --password-file、设置 {env}，或在终端运行",
    ),
    ("Encryption password: ", "加密密码："),
    ("Confirm encryption password: ", "确认加密密码："),
    ("the two passwords differ", "两次输入的密码不一致"),
    ("the encryption password is empty", "加密密码为空"),
    (
        "reading the password file '{path}'",
        "读取密码文件 '{path}' 出错",
    ),
    // cse.py line() pieces:
    (
        "skipped s3://{bucket}/{key} (already encrypted)",
        "已跳过 s3://{bucket}/{key}（已加密）",
    ),
    (
        "skipped s3://{bucket}/{key} (not encrypted)",
        "已跳过 s3://{bucket}/{key}（未加密）",
    ),
    (
        "encrypted s3://{bucket}/{key}",
        "已加密 s3://{bucket}/{key}",
    ),
    (
        "decrypted s3://{bucket}/{key}",
        "已解密 s3://{bucket}/{key}",
    ),
    ("skipped s3://{bucket}/{key}", "已跳过 s3://{bucket}/{key}"),
    // edit.rs
    (
        "{uri} is not an object; edit needs an object, for example s3://{bucket}/key",
        "{uri} 不是对象；edit 需要一个对象，例如 s3://{bucket}/key",
    ),
    ("editing {uri}: {path}", "正在编辑 {uri}：{path}"),
    (
        "Save the file to upload it. Press Ctrl-C to stop editing.",
        "保存文件即会上传。按 Ctrl-C 停止编辑。",
    ),
    ("uploaded {uri}", "已上传 {uri}"),
    (
        "{uri} was deleted since you opened it; your save was not uploaded (the copy stays at {path})",
        "自打开以来 {uri} 已经被删除；你的保存没有上传（副本保留在 {path}）",
    ),
    (
        "{uri} changed on the server since you opened it; your save was not uploaded (the copy stays at {path})",
        "自打开以来 {uri} 在服务器上被更改；你的保存没有上传（副本保留在 {path}）",
    ),
    (
        "{uri} was not uploaded: {message}; save the file again to retry",
        "未能上传 {uri}：{message}；请再次保存文件以重试",
    ),
    (
        "s3://{bucket}/{key} changed on the server. Overwrite it with your version (o), save your version as a copy (c), discard your changes (d), or decide later (l)? ",
        "服务器上的 s3://{bucket}/{key} 已更改。用你的版本覆盖 (o)、把你的版本保存为副本 (c)、放弃你的更改 (d)，还是稍后决定 (l)？ ",
    ),
    (
        "saved your version as s3://{bucket}/{key}",
        "已把你的版本保存为 s3://{bucket}/{key}",
    ),
    (
        "discarded your changes to s3://{bucket}/{key}",
        "已放弃你对 s3://{bucket}/{key} 的更改",
    ),
    (
        "stopped editing s3://{bucket}/{key}",
        "已停止编辑 s3://{bucket}/{key}",
    ),
    (
        "your edited copy is kept at {path}",
        "你编辑的副本保留在 {path}",
    ),
    // ask.rs
    (
        "\n{destination} already exists ({destination_info}); source {source} ({source_info}).\n[o]verwrite, [s]kip, [r]ename, [n]ewer only, [c]ancel; a capital letter applies to all remaining: ",
        "\n{destination} 已存在（{destination_info}）；源 {source}（{source_info}）。\n[o] 覆盖、[s] 跳过、[r] 重命名、[n] 仅当较新、[c] 取消；大写字母表示对剩下的也一并处理： ",
    ),
    ("unknown size", "大小未知"),
    ("unknown time", "时间未知"),
    ("{bytes} bytes", "{bytes} 字节"),
    ("{size}, {time}", "{size}，{time}"),
    (
        "reading the answer failed: {error}",
        "读取回答失败：{error}",
    ),
    ("reading the answer", "读取回答出错"),
    // account.rs
    (
        "--account and --mfa-code do not apply to `bb account`; name the account as an argument",
        "--account 和 --mfa-code 不适用于 `bb account`；请以参数指定账户",
    ),
    (
        "--profile applies to `bb account import-aws` only; add a profile account with it",
        "--profile 只适用于 `bb account import-aws`；用它添加配置文件账户",
    ),
    (
        "no configuration directory: set {env} or pass --config-dir",
        "没有配置目录：请设置 {env} 或传 --config-dir",
    ),
    (
        "account '{selector}' not found; `bb account list` shows the stored accounts",
        "未找到账户 '{selector}'；`bb account list` 可查看已存储的账户",
    ),
    ("--duration is too long", "--duration 过长"),
    ("{flag} is required", "{flag} 是必需的"),
    ("account '{selector}' not found", "未找到账户 '{selector}'"),
    ("do not apply", "不适用于"),
    ("already client-side encrypted", "已经是客户端加密的"),
    ("already", "已"),
    ("skipped s3://", "已跳过 s3://"),
    ("differ", "不一致"),
    ("editing s3://", "正在编辑 s3://"),
    ("stopped editing", "已停止编辑"),
    ("changed on the server", "在服务器上被更改"),
    ("your edited copy is kept at", "你编辑的副本保留在"),
    ("continuation token", "续传标记"),
    ("names a bucket", "是一个存储桶"),
    ("no URL shortener", "未配置短链服务"),
    ("sent to", "会发送到"),
    ("has no versions", "没有任何版本"),
    ("{scanned} buckets scanned", "{scanned} 个存储桶"),
    ("onto itself", "到它自己上面"),
    ("inside the source", "在源的内部"),
    (
        "copying {source} to {destination} would put every object onto itself",
        "把 {source} 复制到 {destination} 会把每个对象都复制到它自己上面",
    ),
    (
        "the destination {destination} is inside the source {source}",
        "目标 {destination} 在源 {source} 的内部",
    ),
    ("saved sync job", "已保存同步任务"),
    ("deleted sync job", "已删除同步任务"),
    ("runs with the account", "使用账户"),
    ("object key", "对象键"),
    ("shown only now", "只显示这一次"),
    ("password: ", "密码："),
    (": valid", "：有效"),
    ("cannot upload", "无法上传"),
    ("is a file", "是一个文件"),
    ("nothing was downloaded", "没有下载任何内容"),
    ("skipped", "已跳过"),
    ("terminal", "终端"),
    ("cannot sync", "无法同步"),
    ("failed to download", "无法下载"),
    ("Resume with: ", "恢复请用："),
    ("try again with: ", "重试请用："),
    ("not done", "未完成"),
    ("interrupted", "已中断"),
    ("required", "必需"),
    ("cannot be used", "不能"),
    ("empty", "为空"),
    ("not a plan ID", "不是计划 ID"),
    ("local folder first", "先取本地文件夹"),
    ("invalid duration", "时长无效"),
    (
        "invalid duration '{text}': use seconds (90) or a number with a unit s, m, h or d (30s, 15m, 1h, 7d, 1h30m)",
        "时长无效 '{text}'：请用秒数（90）或带单位 s、m、h、d 的数字（30s、15m、1h、7d、1h30m）",
    ),
    (
        "'{text}' is out of range: a presigned URL is valid for 1s to 7d ({seconds} seconds)",
        "'{text}' 超出范围：预签名 URL 的有效时长为 1s 到 7d（{seconds} 秒）",
    ),
    ("out of range", "超出范围"),
    ("imported profile as account", "已把配置导入为账户"),
    ("found the role", "发现本机的角色"),
    (
        "found the role of this machine (access key {key})",
        "发现本机的角色（访问密钥 {key}）",
    ),
    ("standard input is unavailable", "标准输入不可用"),
    ("standard input is empty", "标准输入为空"),
    (
        "--secret-stdin: standard input is empty; expected the secret access key on the first line",
        "--secret-stdin：标准输入为空；应在第一行给出秘密访问密钥",
    ),
    ("Secret access key: ", "秘密访问密钥："),
    (
        "no secret access key: use --secret-stdin, set {env}, or run bb in a terminal",
        "没有秘密访问密钥：请用 --secret-stdin、设置 {env}，或在终端运行 bb",
    ),
    ("signed in to", "已登录"),
    ("added account {label}", "已添加账户 {label}"),
    ("added account {label} ({id})", "已添加账户 {label}（{id}）"),
    ("no AWS profile to import", "没有要导入的 AWS 配置"),
    (
        "imported profile as account {label} ({id})",
        "已把配置导入为账户 {label}（{id}）",
    ),
    (
        "skipped profile {profile}: {reason}",
        "已跳过配置 {profile}：{reason}",
    ),
    ("no profile was imported", "没有导入任何配置"),
    (
        "Open {url} and confirm the code {code}",
        "打开 {url} 并确认验证码 {code}",
    ),
    (
        "could not open the browser: {reason}",
        "无法打开浏览器：{reason}",
    ),
    (
        "signed in to {url} until {time}",
        "已登录 {url}，有效期至 {time}",
    ),
    (
        "account '{selector}' does not use IAM Identity Center",
        "账户 '{selector}' 不使用 IAM Identity Center",
    ),
    (
        "name the portal with --start-url and --sso-region, or an SSO account",
        "请用 --start-url 和 --sso-region 或某个 SSO 账户指定门户",
    ),
    (
        "removed account {label} ({id})",
        "已移除账户 {label}（{id}）",
    ),
    (
        "default account is now {label} ({id})",
        "默认账户现在是 {label}（{id}）",
    ),
    ("only https URLs are opened", "只打开 https 的 URL"),
    ("Master password: ", "主密码："),
    (
        "--mfa-code needs a stored account with an MFA device",
        "--mfa-code 需要有 MFA 设备的已存储账户",
    ),
    (
        "--profile cannot be combined with --account or --mfa-code",
        "--profile 不能与 --account 或 --mfa-code 一起使用",
    ),
    (
        "--profile can be given once for a command (several only to `bb account import-aws`)",
        "--profile 对一条命令只能给一次（只有 `bb account import-aws` 可以多个）",
    ),
    (
        "no credentials: set {key_env} and {secret_env}, or add an account with `bb account add`",
        "没有凭据：请设置 {key_env} 和 {secret_env}，或用 `bb account add` 添加账户",
    ),
    (
        "{name} must be true or false (also 1/0, yes/no, on/off), got {value}",
        "{name} 必须是 true 或 false（也可以是 1/0、yes/no、on/off），得到的是 {value}",
    ),
    ("{name} is not valid UTF-8", "{name} 不是有效的 UTF-8"),
    (
        "{path} is not an existing folder",
        "{path} 不是已存在的文件夹",
    ),
    (
        "mounted s3://{bucket} on {path} (read-only); press Ctrl-C to unmount",
        "已把 s3://{bucket} 挂载到 {path}（只读）；按 Ctrl-C 卸载",
    ),
    // scan.rs
    (
        "{severity} {bucket}  {title}  [fix: {tab} tab]\n         {explanation}",
        "{severity} {bucket}  {title}  [修复：{tab} 标签页]\n         {explanation}",
    ),
    (
        "skipped  {bucket}  {message}",
        "已跳过  {bucket}  {message}",
    ),
    (
        "error    {bucket}  {message}",
        "错误    {bucket}  {message}",
    ),
    (
        "{scanned} buckets scanned: {critical} critical, {high} high, {medium} medium, {low} low",
        "已扫描 {scanned} 个存储桶：{critical} 个 critical、{high} 个 high、{medium} 个 medium、{low} 个 low",
    ),
    // mpu.rs
    (
        "aborted upload {id} of s3://{bucket}/{key}",
        "已中止 {id} 在 s3://{bucket}/{key} 的上传",
    ),
    (
        "would abort upload {id} of s3://{bucket}/{key}",
        "（试运行）将中止 {id} 在 s3://{bucket}/{key} 的上传",
    ),
    ("objects: {count}, {size}", "对象：{count} 个，{size}"),
    (
        "non-current versions: {count}, {size}",
        "非当前版本：{count} 个，{size}",
    ),
    ("delete markers: {count}", "删除标记：{count} 个"),
    (
        "unfinished multipart uploads: {count} ({parts} parts), {size}",
        "未完成的分段上传：{count} 个（{parts} 个分段），{size}",
    ),
    (
        "an account cannot be moved before itself",
        "账户不能移到它自己之前",
    ),
    (
        "moved account {label} to folder {folder}",
        "已将账户 {label} 移到文件夹 {folder}",
    ),
    (
        "moved account {label} (no folder)",
        "已移动账户 {label}（不在文件夹中）",
    ),
    (
        "a folder named {name} already exists",
        "已存在名为 {name} 的文件夹",
    ),
    ("added folder {name}", "已添加文件夹 {name}"),
    (
        "folder '{name}' not found; `bb account folder list` shows the folders",
        "未找到文件夹“{name}”；`bb account folder list` 会列出文件夹",
    ),
    (
        "renamed folder {old} to {name}",
        "已将文件夹 {old} 重命名为 {name}",
    ),
    (
        "removed folder {name} (its accounts are kept)",
        "已删除文件夹 {name}（其中的账户已保留）",
    ),
    (
        "wrote {count} account(s) to {file} (no secrets: imported accounts need their secret access key)",
        "已把 {count} 个账户写入 {file}（不含机密：导入的账户需要输入其 Secret Access Key）",
    ),
    (
        "imported account {label}: it needs its secret access key before it can be used (bb account set-secret)",
        "已导入账户 {label}：使用前需要输入其 Secret Access Key（bb account set-secret）",
    ),
    ("imported account {label}", "已导入账户 {label}"),
    (
        "skipped {label}: an account with this label exists (--rename-duplicates imports it under another label)",
        "已跳过 {label}：已存在相同标签的账户（--rename-duplicates 会改用其他标签导入）",
    ),
    (
        "skipped {label}: it is the same account as {existing}",
        "已跳过 {label}：它与 {existing} 是同一个账户",
    ),
    ("skipped {label}: {reason}", "已跳过 {label}：{reason}"),
    (
        "{imported} account(s) imported, {duplicates} duplicate(s) left out, {skipped} skipped",
        "已导入 {imported} 个账户，{duplicates} 个重复已忽略，{skipped} 个已跳过",
    ),
    ("  [folder: {folder}]", "  [文件夹：{folder}]"),
    (
        "  (needs its secret access key)",
        "  （需要输入 Secret Access Key）",
    ),
    (
        "account {label} gets its credentials from another source and has no secret access key",
        "账户 {label} 的凭据来自其他来源，没有 Secret Access Key",
    ),
    (
        "stored the secret access key of account {label} ({id})",
        "已保存账户 {label}（{id}）的 Secret Access Key",
    ),
    (
        "failed to abort upload {id} of s3://{bucket}/{key}: {error}",
        "无法中止 {id} 在 s3://{bucket}/{key} 的上传：{error}",
    ),
    (
        "give --key and --upload-id, or --all",
        "请给出 --key 和 --upload-id，或 --all",
    ),
    // objectmeta.rs
    (
        "{uri} is a prefix; add --recursive to change every object under it",
        "{uri} 是一个前缀；请加 --recursive 以更改其下所有对象",
    ),
    (
        "--recursive needs a prefix ending in '/', for example {prefixish}/",
        "--recursive 需要以 '/' 结尾的前缀，例如 {prefixish}/",
    ),
    (
        "failed to {action} {uri}: {message}",
        "无法对 {uri} {action}：{message}",
    ),
    (
        "bb: warning: {uri}: {warning}",
        "bb: warning: {uri}：{warning}",
    ),
    ("tag", "设置标签"),
    ("remove tags from", "移除标签"),
    ("change the storage class of", "更改存储类型"),
    ("change the encryption of", "更改加密"),
    ("set the ACL of", "设置 ACL"),
    ("restore", "恢复"),
    ("change the retention of", "更改保留"),
    ("change the legal hold of", "更改法律保留"),
    ("key file", "密钥文件"),
    ("source key file", "源密钥文件"),
    ("reading the {what} {path}", "读取{what} {path} 出错"),
    ("the {what} {path}", "{what} {path}"),
    ("would tag {uri}", "（试运行）将设置标签 {uri}"),
    ("tagged {uri}", "已设置标签 {uri}"),
    ("unchanged {uri}", "无更改 {uri}"),
    (
        "unchanged {uri} (already {what})",
        "无更改 {uri}（已是 {what}）",
    ),
    (
        "would remove tags from {uri}",
        "（试运行）将移除 {uri} 的标签",
    ),
    ("removed tags from {uri}", "已移除 {uri} 的标签"),
    (
        "failed to read the tags of {uri}: {error}",
        "无法读取 {uri} 的标签：{error}",
    ),
    (
        "would change {uri} from {previous} to {class}",
        "（试运行）将把 {uri} 从 {previous} 更改为 {class}",
    ),
    (
        "changed {uri} from {previous} to {class}",
        "已把 {uri} 从 {previous} 更改为 {class}",
    ),
    (
        "SSE-C (c) needs the new key: pass --key-file FILE",
        "SSE-C（c）需要新密钥：请传 --key-file FILE",
    ),
    (
        "--key-file is the key of SSE-C (c); use --source-key-file for the key the objects have now",
        "--key-file 是 SSE-C（c）的密钥；对象现有的密钥请用 --source-key-file",
    ),
    (
        "--bucket-key only applies to kms",
        "--bucket-key 只适用于 kms",
    ),
    ("SSE-C (c) needs --key-file", "SSE-C（c）需要 --key-file"),
    (
        "would change the encryption of {uri} from {previous} to {name}",
        "（试运行）将把 {uri} 的加密从 {previous} 更改为 {name}",
    ),
    (
        "changed the encryption of {uri} from {previous} to {name}",
        "已把 {uri} 的加密从 {previous} 更改为 {name}",
    ),
    (
        "failed to read the ACL of {uri}: {error}",
        "无法读取 {uri} 的 ACL：{error}",
    ),
    (" (was {previous})", "（原来是 {previous}）"),
    (
        "would set the ACL of {uri} to {acl}{was}",
        "（试运行）将把 {uri} 的 ACL 设为 {acl}{was}",
    ),
    (
        "set the ACL of {uri} to {acl}{was}",
        "已把 {uri} 的 ACL 设为 {acl}{was}",
    ),
    ("none", "无"),
    // objectlock.rs
    (
        "--bypass-governance applies to a retention change (--mode and --until, or --clear-retention)",
        "--bypass-governance 只适用于保留更改（--mode 与 --until，或 --clear-retention）",
    ),
    ("--until must be in the future", "--until 必须是将来时间"),
    (
        "failed to read the lock of {uri}: {error}",
        "无法读取 {uri} 的锁：{error}",
    ),
    (
        "the retention of {uri} to {mode} until {until}",
        "{uri} 的保留改为 {mode}，至 {until}",
    ),
    ("the retention of {uri} to none", "{uri} 的保留改为无"),
    ("would set {what}", "（试运行）将设置 {what}"),
    ("set {what}", "已设置 {what}"),
    (
        "would turn the legal hold of {uri} {word}",
        "（试运行）将把 {uri} 的法律保留改为 {word}",
    ),
    (
        "turned the legal hold of {uri} {word}",
        "已把 {uri} 的法律保留改为 {word}",
    ),
    (
        "unchanged {uri} (legal hold already {word})",
        "无更改 {uri}（法律保留已是 {word}）",
    ),
    ("on", "开"),
    ("off", "关"),
    (
        "would request the restore of {uri} for {days} days ({tier})",
        "（试运行）将请求从 Glacier 恢复 {uri}，持续 {days} 天（{tier}）",
    ),
    (
        "requested the restore of {uri} for {days} days ({tier})",
        "已请求恢复 {uri}，持续 {days} 天（{tier}）",
    ),
    // presign.rs
    (
        "{uri} names a bucket; presign needs an object, for example s3://{bucket}/key",
        "{uri} 是一个存储桶；presign 需要一个对象，例如 s3://{bucket}/key",
    ),
    (
        "note: the link, including its signature, is sent to {provider}",
        "注意：该链接（包括其签名）会发送到 {provider}",
    ),
    (
        "no URL shortener is configured: choose one in the settings (the desktop app's Settings, or `provider` under [shortener] in config.toml)",
        "未配置短链服务：请在设置中选择一个（桌面应用的 Settings，或 config.toml 中 [shortener] 下的 `provider`）",
    ),
    // export.rs
    ("1 row", "1 行"),
    ("{rows} rows", "{rows} 行"),
    (
        "exported {rows} from {source} to {file} ({format}, {bytes} bytes)",
        "已导出 {rows}，来源 {source}，写入 {file}（{format}，{bytes} 字节）",
    ),
    ("resolving {path}", "解析 {path} 出错"),
    // iam.rs
    (
        "deleting user {name} and everything it owns",
        "删除用户 {name} 及其所有内容",
    ),
    ("deleting access key {key_id}", "删除访问密钥 {key_id}"),
    (
        "deleting the inline policy {policy}",
        "删除内联策略 {policy}",
    ),
    (
        "removing the console password of {name}",
        "移除 {name} 的控制台密码",
    ),
    ("created user {name} ({arn})", "已创建用户 {name}（{arn}）"),
    ("updated user {name} ({arn})", "已更新用户 {name}（{arn}）"),
    (
        "bb: failed to remove {kind} {target}: {message}",
        "bb: 无法移除 {kind} {target}：{message}",
    ),
    ("deleted user {name}", "已删除用户 {name}"),
    ("did not delete user {name}", "没有删除用户 {name}"),
    (
        "user {name} was not deleted: {message}",
        "未能删除用户 {name}：{message}",
    ),
    ("  (none)", "  （无）"),
    ("  (not available: {message})", "  （不可用：{message}）"),
    (
        "  (could not be read: {message})",
        "  （无法读取：{message}）",
    ),
    ("{title}:", "{title}："),
    ("managed policies", "托管策略"),
    ("inline policies", "内联策略"),
    ("access keys", "访问密钥"),
    ("console password", "控制台密码"),
    ("groups", "组"),
    ("user: {name}", "用户：{name}"),
    ("arn: {arn}", "ARN：{arn}"),
    ("path: {path}", "路径：{path}"),
    ("created: {time}", "创建时间：{time}"),
    ("password last used: {time}", "密码上次使用：{time}"),
    ("id: {id}", "ID：{id}"),
    ("set {time}{reset}", "设置于 {time}{reset}"),
    ("status: {status}", "状态：{status}"),
    ("state: {state}", "状态：{state}"),
    ("comment: {comment}", "备注：{comment}"),
    ("price class: {price_class}", "价格层级：{price_class}"),
    ("last modified: {time}", "最后修改：{time}"),
    ("aliases: {aliases}", "别名：{aliases}"),
    ("default root object: {root}", "根 URL 对象：{root}"),
    (
        "invalidations in progress: {count}",
        "进行中的失效：{count}",
    ),
    ("origin {id}: {domain}", "源 {id}：{domain}"),
    (
        "origin {id}: {domain} (bucket {bucket})",
        "源 {id}：{domain}（存储桶 {bucket}）",
    ),
    (" (bucket {bucket}", "（存储桶 {bucket}"),
    (", website endpoint", "，网站端点"),
    (", origin access control", "，源访问控制"),
    (")", "）"),
    (
        ", must be changed at the next sign-in",
        "，必须在下次登录时修改",
    ),
    (
        "must be changed at the next sign-in",
        "必须在下次登录时修改",
    ),
    (
        "user: {user}\naccess key ID: {key_id}\nsecret access key: {secret}",
        "用户：{user}\n访问密钥 ID：{key_id}\n秘密访问密钥：{secret}",
    ),
    (
        "bb: warning: the secret access key is shown only now; IAM cannot show it again",
        "bb: warning: 秘密访问密钥只显示这一次；IAM 不会再显示它",
    ),
    (
        "bb: warning: the generated password is shown only now; it cannot be read again",
        "bb: warning: 生成的密码只显示这一次；之后再也无法查看",
    ),
    (
        "deleted access key {key_id} of {user}",
        "已删除用户 {user} 的访问密钥 {key_id}",
    ),
    (
        "activated access key {key_id} of {user}",
        "已激活用户 {user} 的访问密钥 {key_id}",
    ),
    (
        "deactivated access key {key_id} of {user}",
        "已停用用户 {user} 的访问密钥 {key_id}",
    ),
    ("attached {policy} to {user}", "已把 {policy} 附加到 {user}"),
    (
        "detached {policy} from {user}",
        "已把 {policy} 从 {user} 分离",
    ),
    (
        "saved inline policy {policy} of {user}",
        "已保存用户 {user} 的内联策略 {policy}",
    ),
    (
        "deleted inline policy {policy} of {user}",
        "已删除用户 {user} 的内联策略 {policy}",
    ),
    ("{name}: {what}", "{name}：{what}"),
    ("{name}: no console password", "{name}：没有控制台密码"),
    (
        "removed the console password of {user}",
        "已移除 {user} 的控制台密码",
    ),
    (
        "updated the console password settings of {user}",
        "已更新 {user} 的控制台密码设置",
    ),
    (
        "{verb} the console password of {user}",
        "{verb} {user} 的控制台密码",
    ),
    (
        "{verb} the console password of {user}\npassword: {password}",
        "{verb} {user} 的控制台密码\n密码：{password}",
    ),
    ("created", "已创建"),
    ("updated", "已更新"),
    ("removed", "已移除"),
    ("no password", "未提供密码"),
    (
        "--password-stdin: standard input is empty; expected the password on the first line",
        "--password-stdin：标准输入为空；应在第一行给出密码",
    ),
    ("Console password: ", "控制台密码："),
    (
        "no password: use --password-stdin or --generate, or run bb in a terminal",
        "没有密码：请用 --password-stdin 或 --generate，或在终端运行 bb",
    ),
    ("{name}: valid", "{name}：有效"),
    ("{name}: {message}", "{name}：{message}"),
    ("{name}: {path}: {message}", "{name}：{path}：{message}"),
    (
        "{count} problem(s) found in {name}",
        "在 {name} 中发现 {count} 个问题",
    ),
    // cloudfront.rs
    (
        "created distribution {id} ({domain}) for bucket {bucket}\nThe bucket is not changed. Add this statement to the bucket policy of {policy_bucket} so that the distribution can read it:\n{policy}",
        "已为存储桶 {bucket} 创建分配（CloudFront）{id}（{domain}）\n存储桶没有更改。请把这条语句添加到 {policy_bucket} 的存储桶策略中，使分配（CloudFront）可以读取它：\n{policy}",
    ),
    (
        "created distribution {id} ({domain}) for bucket {bucket}\n",
        "已为存储桶 {bucket} 创建分配（CloudFront）{id}（{domain}）\n",
    ),
    ("enabled distribution {id}", "已启用分配（CloudFront）{id}"),
    ("disabled distribution {id}", "已停用分配（CloudFront）{id}"),
    (
        "distribution {id} no longer serves bucket {bucket}",
        "分配（CloudFront）{id} 不再服务存储桶 {bucket}",
    ),
    (
        "created invalidation {id} ({status}): {paths}",
        "已创建失效 {id}（{status}）：{paths}",
    ),
    (
        "the region of bucket {bucket} is unknown: pass --region",
        "存储桶 {bucket} 的区域未知：请传 --region",
    ),
    // restore.rs
    (
        "restored version {version} of s3://{bucket}/{key} (new version {new_version})",
        "已恢复 s3://{bucket}/{key} 的版本 {version}（新版本 {new_version}）",
    ),
    (
        "restored version {version} of s3://{bucket}/{key}",
        "已恢复 s3://{bucket}/{key} 的版本 {version}",
    ),
    (
        "restore-version needs an object key; {uri} is a prefix",
        "restore-version 需要对象键；{uri} 是一个前缀",
    ),
    // syncjobs.rs
    (
        "--account and --mfa-code do not apply to `bb sync jobs`; a job names its account",
        "--account 和 --mfa-code 不适用于 `bb sync jobs`；任务的账户由任务指定",
    ),
    (
        "deleting the sync job '{name}' needs --yes",
        "删除同步任务 '{name}' 需要 --yes",
    ),
    ("deleted sync job {name}", "已删除同步任务 {name}"),
    ("name: {name}", "名称：{name}"),
    ("account: {account}", "账户：{account}"),
    ("direction: {direction}", "方向：{direction}"),
    ("from: {from}", "来源：{from}"),
    ("to: {to}", "目标：{to}"),
    ("delete: {flag}", "删除：{flag}"),
    ("include: {list}", "包含：{list}"),
    ("exclude: {list}", "排除：{list}"),
    ("exclude file: {file}", "排除规则文件：{file}"),
    ("skip empty folders: {flag}", "跳过空文件夹：{flag}"),
    ("skip duplicate content: {flag}", "跳过重复内容：{flag}"),
    ("use cache: {flag}", "使用缓存：{flag}"),
    ("parallelism: {count}", "并行数：{count}"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::CommandFactory;

    #[test]
    fn language_follows_the_override() {
        with_language(true, || assert_eq!(tr("copied"), "copied"));
        with_language(false, || assert_eq!(tr("copied"), tr("copied")));
    }

    #[test]
    fn unknown_text_passes_through_but_registered_text_translates() {
        with_language(false, || {
            assert_eq!(
                tr("nothing has registered this"),
                "nothing has registered this"
            );
            assert_eq!(
                tr(
                    "Print nothing for successful changes and no progress; failures are still reported."
                ),
                "成功的更改不输出任何内容，也不显示进度；失败仍会报告。"
            );
        });
    }

    #[test]
    fn whitespace_is_normalized_before_the_lookup() {
        with_language(false, || {
            for source in [
                // one long line with single or double blanks,
                "created bucket {bucket}  in {region}",
                // wrapped, like a doc comment,
                "created bucket {bucket}\n  in {region}",
                // or with tabs.
                "\tcreated bucket {bucket} in {region}\t",
            ] {
                assert_eq!(tr(source), "已创建存储桶 {bucket}（{region}）", "{source}");
            }
        });
    }

    #[test]
    fn trf_fills_named_placeholders() {
        with_language(false, || {
            assert_eq!(
                trf(
                    "created bucket {bucket} in {region}",
                    &[("bucket", "photos"), ("region", "eu-west-1"),]
                ),
                "已创建存储桶 photos（eu-west-1）"
            );
        });
        with_language(true, || {
            assert_eq!(
                trf(
                    "created bucket {bucket} in {region}",
                    &[("bucket", "b"), ("region", "r")]
                ),
                "created bucket b in r"
            );
        });
    }

    #[test]
    fn trf_fills_placeholders_in_one_pass() {
        // A value that itself looks like a placeholder is inserted as it is;
        // an S3 key can contain `{name}`.
        with_language(true, || {
            assert_eq!(trf("{a} {b}", &[("a", "{b}"), ("b", "B")]), "{b} B");
            assert_eq!(trf("key {key}", &[("key", "a{b}c")]), "key a{b}c");
        });
    }

    #[test]
    fn the_locale_chooses_the_language() {
        let vars = |pairs: &[(&str, &str)]| {
            let map: HashMap<String, String> = pairs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect();
            move |name: &str| map.get(name).cloned()
        };
        // No locale at all is English, as on Windows without a language.
        assert!(english_from(vars(&[])));
        assert!(english_from(vars(&[("LANG", "en_US.UTF-8")])));
        assert!(english_from(vars(&[("LANG", "C")])));
        assert!(english_from(vars(&[("LANG", "de_DE.UTF-8")])));
        // Chinese only for a zh locale.
        assert!(!english_from(vars(&[("LANG", "zh_CN.UTF-8")])));
        assert!(!english_from(vars(&[("LC_ALL", "zh_SG.UTF-8")])));
        // `LC_ALL` wins over `LANG`, `BB_LANG` wins over both.
        assert!(!english_from(vars(&[
            ("LC_ALL", "zh_CN.UTF-8"),
            ("LANG", "en_US.UTF-8"),
        ])));
        assert!(!english_from(vars(&[
            ("BB_LANG", "zh"),
            ("LC_ALL", "en_US.UTF-8"),
        ])));
        assert!(english_from(vars(&[
            ("BB_LANG", "en"),
            ("LANG", "zh_CN.UTF-8"),
        ])));
    }

    #[test]
    fn the_table_has_no_duplicate_keys() {
        let mut keys: Vec<String> = std::iter::once(&AFTER_ENTRY)
            .chain(HELP.iter())
            .chain(OUTPUT.iter())
            .map(|(source, _)| normalize(source))
            .collect();
        keys.sort();
        let duplicates: Vec<String> = keys
            .windows(2)
            .filter(|pair| pair[0] == pair[1])
            .map(|pair| pair[0].clone())
            .collect();
        assert!(duplicates.is_empty(), "duplicate keys: {duplicates:#?}");
    }

    #[test]
    fn translate_command_rewrites_the_root_about_and_a_flag() {
        with_language(false, || {
            let mut cmd = Cli::command();
            let original = cmd.get_about().map(|text| text.to_string()).unwrap();
            translate_command(&mut cmd);
            let translated = cmd.get_about().map(|text| text.to_string()).unwrap();
            assert_ne!(translated, original, "the about should be Chinese");
            assert_eq!(translated, tr(&original));
            let quiet = cmd
                .get_arguments()
                .find(|arg| arg.get_id().as_str() == "quiet")
                .map(|arg| arg.get_help().map(|text| text.to_string()).unwrap())
                .unwrap();
            assert_eq!(
                quiet,
                tr(
                    "Print nothing for successful changes and no progress; failures are still reported."
                )
            );
        });
    }

    #[test]
    fn translate_command_walks_the_subcommands() {
        with_language(false, || {
            let mut cmd = Cli::command();
            translate_command(&mut cmd);
            let help = cmd.render_help().to_string();
            assert!(
                help.contains("列出存储桶，或 S3 前缀下的文件夹与对象。"),
                "{help}"
            );
            // The English original of the `ls` about is gone.
            assert!(!help.contains("List buckets, or the folders and objects under an S3 prefix."));
        });
    }

    #[test]
    fn translate_command_is_a_noop_in_english() {
        with_language(true, || {
            let mut cmd = Cli::command();
            let original = cmd.get_about().map(|text| text.to_string()).unwrap();
            translate_command(&mut cmd);
            assert_eq!(
                cmd.get_about().map(|text| text.to_string()).as_deref(),
                Some(original.as_str())
            );
        });
    }

    #[test]
    fn every_help_text_of_the_definition_is_registered() {
        with_language(false, || {
            fn walk(cmd: &clap::Command, miss: &mut Vec<String>) {
                for field in [
                    ("about", cmd.get_about().map(|t| t.to_string())),
                    ("long_about", cmd.get_long_about().map(|t| t.to_string())),
                    ("after_help", cmd.get_after_help().map(|t| t.to_string())),
                    (
                        "after_long_help",
                        cmd.get_after_long_help().map(|t| t.to_string()),
                    ),
                ] {
                    if let Some(text) = field.1
                        && text == tr(&text)
                        && !text.is_empty()
                    {
                        miss.push(format!("{:?}: {text}", cmd.get_name()));
                    }
                }
                for arg in cmd.get_arguments() {
                    for (kind, text) in [
                        ("help", arg.get_help().map(|t| t.to_string())),
                        ("long_help", arg.get_long_help().map(|t| t.to_string())),
                    ] {
                        if let Some(text) = text
                            && text == tr(&text)
                            && !text.contains("possible values")
                        {
                            miss.push(format!("{kind}: {text}"));
                        }
                    }
                }
                for sub in cmd.get_subcommands() {
                    walk(sub, miss);
                }
            }
            let mut missed = Vec::new();
            walk(&Cli::command(), &mut missed);
            assert!(missed.is_empty(), "unregistered help texts: {missed:#?}");
        });
    }

    #[test]
    fn entry_count_counts_registered_entries() {
        assert!(entry_count() > 100);
    }
}
