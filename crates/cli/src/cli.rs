//! The command-line definition (clap).

use std::path::PathBuf;
use std::time::Duration;

use bucketbridge_core::cse::Compression;
use bucketbridge_core::export::Format as ExportFormat;
use bucketbridge_core::filter::{Glob, IncludeExclude};
use bucketbridge_core::model::LockMode;
use bucketbridge_core::model::{DEFAULT_REGION, SyncDirection};
use bucketbridge_core::objectlock::{self, RestoreTier};
use bucketbridge_core::objectmeta::{CannedAcl, StorageClass, validate_tag};
use bucketbridge_core::settings::IfExists;
use bucketbridge_core::{Error, S3Uri};
use chrono::{DateTime, NaiveDate, Utc};
use clap::{ArgGroup, ArgMatches, Args, Parser, Subcommand, ValueEnum};

use crate::duration::parse_expiry;
use crate::location::{Location, parse_bucket, parse_location, parse_s3_uri};

const AFTER_HELP: &str = "\
Accounts: --account picks a stored account by label or ID; without it the
default account is used, and without one the environment account:
  BB_ACCESS_KEY_ID      access key ID
  BB_SECRET_ACCESS_KEY  secret access key
  BB_SESSION_TOKEN      session token for temporary credentials
  BB_REGION             signing region (default us-east-1)
  BB_ENDPOINT           custom endpoint URL for S3-compatible providers
  BB_PATH_STYLE         use path-style addressing (true/false)
Other variables:
  BB_MASTER_PASSWORD    master password for stored secrets (else asked on the terminal)
  BB_CSE_PASSWORD       password of --encrypt / --decrypt and bb encrypt / decrypt (else --password-file, else asked on the terminal)
  BB_CONFIG_DIR         directory holding config.toml (same as --config-dir)
  BB_SHORTENER_TOKEN    API token of the URL shortener for `presign --shorten`

Exit codes: 0 ok, 1 usage error, 2 remote or local failure (also not found and
conflicts), 3 authentication failure or locked secrets, 130 interrupted.";

/// BucketBridge command-line S3 client.
#[derive(Debug, Parser)]
#[command(name = "bb", version, about, after_help = AFTER_HELP, arg_required_else_help = true)]
pub struct Cli {
    /// Options accepted by every command.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Options accepted before or after any command.
#[derive(Debug, Clone, Args)]
pub struct GlobalArgs {
    /// Account to use, by label or ID (`environment` is the account built from the BB_* variables).
    #[arg(long, global = true, value_name = "LABEL|ID")]
    pub account: Option<String>,
    /// Use a profile of the AWS config and credentials files (honours
    /// AWS_CONFIG_FILE and AWS_SHARED_CREDENTIALS_FILE) without storing it. For
    /// `bb account import-aws`, the profiles to import (repeatable).
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Vec<String>,
    /// Current code of the account's MFA device; logs in before the command runs.
    #[arg(long, global = true, value_name = "CODE")]
    pub mfa_code: Option<String>,
    /// Print one JSON record per item (NDJSON); errors become JSON records on stderr.
    #[arg(long, global = true)]
    pub json: bool,
    /// Print nothing for successful changes and no progress; failures are still reported.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Directory holding config.toml (overrides BB_CONFIG_DIR).
    #[arg(long, global = true, value_name = "DIR")]
    pub config_dir: Option<PathBuf>,
}

/// The commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List buckets, or the folders and objects under an S3 prefix.
    Ls(LsArgs),
    /// Make a bucket.
    Mb(MbArgs),
    /// Remove an empty bucket.
    Rb(RbArgs),
    /// Delete objects, folders or object versions.
    Rm(RmArgs),
    /// Make an older version of an object its current version (a server-side copy).
    RestoreVersion(RestoreVersionArgs),
    /// Copy files and objects: local to S3, S3 to local, or S3 to S3.
    Cp(CopyArgs),
    /// Move files and objects (each source is deleted after its copy succeeded).
    Mv(CopyArgs),
    /// Make a local folder and an S3 prefix match (upload, download or both ways).
    Sync(SyncArgs),
    /// Create a presigned URL for downloading or uploading one object.
    Presign(PresignArgs),
    /// List and abort uncompleted multipart uploads.
    #[command(subcommand)]
    Mpu(MpuCommand),
    /// Security scan: public ACLs and policies, public access block, encryption, versioning, object ownership.
    Scan(ScanArgs),
    /// Mount a bucket read-only as a folder or drive (FUSE, Dokan on Windows; needs a build with the mount feature).
    #[cfg(feature = "mount")]
    Mount(MountArgs),
    /// Write the listing of an S3 prefix to a CSV, Parquet or Excel (XLSX) file.
    Export(ExportArgs),
    /// Manage stored accounts (secrets live in the OS keychain).
    #[command(subcommand)]
    Account(AccountCommand),
    /// Read, set and delete object tags.
    #[command(subcommand)]
    Tag(TagCommand),
    /// Change the storage class of objects (a server-side copy onto themselves).
    StorageClass(StorageClassArgs),
    /// Change the server-side encryption of objects (a server-side copy onto themselves).
    Sse(SseArgs),
    /// Read and set the ACL of objects.
    #[command(subcommand)]
    Acl(AclCommand),
    /// Read and set the Object Lock retention and legal hold of objects.
    #[command(subcommand)]
    Lock(LockCommand),
    /// Ask S3 to restore archived (Glacier) objects.
    Restore(RestoreArgs),
    /// Manage CloudFront distributions and invalidations (AWS accounts only).
    #[command(subcommand)]
    Cloudfront(CloudfrontCommand),
    /// Manage IAM users, access keys, policies and console passwords (AWS accounts only).
    #[command(subcommand)]
    Iam(IamCommand),
    /// Encrypt objects in place with a password (client-side encryption).
    Encrypt(EncryptArgs),
    /// Decrypt client-side encrypted objects in place.
    Decrypt(DecryptArgs),
    /// Edit an object in place: open a local copy and upload every save until Ctrl-C.
    Edit(EditArgs),
}

impl Command {
    /// Whether the command moves data through the transfer engines (or, for
    /// `export`, writes a file), which clean up after themselves when
    /// cancelled.
    pub fn is_transfer(&self) -> bool {
        matches!(
            self,
            Command::Cp(_)
                | Command::Mv(_)
                | Command::Sync(_)
                | Command::Export(_)
                | Command::Encrypt(_)
                | Command::Decrypt(_)
        )
    }
}

/// `bb iam`
#[derive(Debug, Subcommand)]
pub enum IamCommand {
    /// List the IAM users of the account.
    Users(IamUsersArgs),
    /// Show, create, update and delete one user.
    #[command(subcommand)]
    User(IamUserCommand),
    /// List, create, enable, disable and delete the access keys of a user.
    #[command(subcommand)]
    Keys(IamKeysCommand),
    /// List managed policies and attach or detach them.
    #[command(subcommand)]
    Policy(IamPolicyCommand),
    /// List, read, write and delete the inline policies of a user.
    #[command(subcommand)]
    Inline(IamInlineCommand),
    /// Show, create, update and delete the console password of a user.
    #[command(subcommand)]
    LoginProfile(IamLoginProfileCommand),
    /// Check a user policy document (a file, or `-` for standard input) without an account.
    ValidatePolicy(IamPolicyFileArgs),
    /// Pretty-print a policy document (a file, or `-` for standard input) without an account.
    FormatPolicy(IamPolicyFileArgs),
}

/// `bb iam users`
#[derive(Debug, Args)]
pub struct IamUsersArgs {
    /// Only users whose path starts with this prefix.
    #[arg(long, value_name = "PREFIX")]
    pub path_prefix: Option<String>,
}

/// A user, named as an argument.
#[derive(Debug, Args)]
pub struct IamUserName {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
}

/// `bb iam user`
#[derive(Debug, Subcommand)]
pub enum IamUserCommand {
    /// Show a user with its policies, access keys, console password and groups.
    Show(IamUserName),
    /// Create a user.
    Create(IamUserCreateArgs),
    /// Delete a user and everything it owns (keys, policies, console password, groups); needs --yes.
    Delete(IamUserDeleteArgs),
    /// Rename a user and/or change its path.
    Update(IamUserUpdateArgs),
}

/// `bb iam user create`
#[derive(Debug, Args)]
pub struct IamUserCreateArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// The path of the user (default `/`).
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,
}

/// `bb iam user delete`
#[derive(Debug, Args)]
pub struct IamUserDeleteArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// Confirm the deletion.
    #[arg(long)]
    pub yes: bool,
}

/// `bb iam user update`
#[derive(Debug, Args)]
pub struct IamUserUpdateArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// Rename the user.
    #[arg(long, value_name = "NAME")]
    pub new_name: Option<String>,
    /// Move the user to another path.
    #[arg(long, value_name = "PATH")]
    pub new_path: Option<String>,
}

/// `bb iam keys`
#[derive(Debug, Subcommand)]
pub enum IamKeysCommand {
    /// List the access keys of a user (IAM never shows their secrets).
    List(IamUserName),
    /// Create an access key. The secret is printed once and cannot be read again.
    Create(IamUserName),
    /// Delete an access key; needs --yes.
    Delete(IamKeyDeleteArgs),
    /// Make an access key usable.
    Activate(IamKeyArgs),
    /// Keep an access key but refuse it.
    Deactivate(IamKeyArgs),
}

/// A user and one of its access keys.
#[derive(Debug, Args)]
pub struct IamKeyArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// The access key ID.
    #[arg(value_name = "KEY_ID")]
    pub key_id: String,
}

/// `bb iam keys delete`
#[derive(Debug, Args)]
pub struct IamKeyDeleteArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// The access key ID.
    #[arg(value_name = "KEY_ID")]
    pub key_id: String,
    /// Confirm the deletion.
    #[arg(long)]
    pub yes: bool,
}

/// `bb iam policy`
#[derive(Debug, Subcommand)]
pub enum IamPolicyCommand {
    /// List the managed policies that can be attached.
    ListManaged(IamListManagedArgs),
    /// List the managed policies attached to a user.
    List(IamUserName),
    /// Attach a managed policy to a user.
    Attach(IamPolicyArnArgs),
    /// Detach a managed policy from a user.
    Detach(IamPolicyArnArgs),
}

/// Which managed policies `bb iam policy list-managed` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IamScopeArg {
    /// The policies of this account.
    Local,
    /// The policies AWS manages.
    Aws,
    /// Both.
    All,
}

/// `bb iam policy list-managed`
#[derive(Debug, Args)]
pub struct IamListManagedArgs {
    /// Which policies to list.
    #[arg(long, value_enum, default_value = "local")]
    pub scope: IamScopeArg,
}

/// A user and a managed policy.
#[derive(Debug, Args)]
pub struct IamPolicyArnArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// The ARN of the managed policy.
    #[arg(value_name = "POLICY_ARN")]
    pub policy_arn: String,
}

/// `bb iam inline`
#[derive(Debug, Subcommand)]
pub enum IamInlineCommand {
    /// List the names of the inline policies of a user.
    List(IamUserName),
    /// Print an inline policy document.
    Get(IamInlineArgs),
    /// Create or replace an inline policy from a file (`-` is standard input).
    Put(IamInlinePutArgs),
    /// Delete an inline policy; needs --yes.
    Delete(IamInlineDeleteArgs),
}

/// A user and one of its inline policies.
#[derive(Debug, Args)]
pub struct IamInlineArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// The policy name.
    #[arg(value_name = "POLICY")]
    pub policy: String,
}

/// `bb iam inline put`
#[derive(Debug, Args)]
pub struct IamInlinePutArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// The policy name.
    #[arg(value_name = "POLICY")]
    pub policy: String,
    /// The policy document (`-` reads standard input).
    #[arg(long, value_name = "FILE")]
    pub file: PathBuf,
}

/// `bb iam inline delete`
#[derive(Debug, Args)]
pub struct IamInlineDeleteArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// The policy name.
    #[arg(value_name = "POLICY")]
    pub policy: String,
    /// Confirm the deletion.
    #[arg(long)]
    pub yes: bool,
}

/// `bb iam login-profile`
#[derive(Debug, Subcommand)]
pub enum IamLoginProfileCommand {
    /// Show whether a user has a console password.
    Show(IamUserName),
    /// Give a user a console password (from standard input, generated, or asked on the terminal).
    Create(IamLoginCreateArgs),
    /// Change the console password of a user and/or whether it must be changed at the next sign-in.
    Update(IamLoginUpdateArgs),
    /// Remove the console password of a user; needs --yes.
    Delete(IamLoginDeleteArgs),
}

/// `bb iam login-profile create`
#[derive(Debug, Args)]
pub struct IamLoginCreateArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// Read the password from the first line of standard input.
    #[arg(long, conflicts_with = "generate")]
    pub password_stdin: bool,
    /// Generate a password and print it once.
    #[arg(long)]
    pub generate: bool,
    /// The user must choose a new password at the next sign-in.
    #[arg(long)]
    pub reset_required: bool,
}

/// `bb iam login-profile update`
#[derive(Debug, Args)]
pub struct IamLoginUpdateArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// Set a new password read from the first line of standard input.
    #[arg(long, conflicts_with = "generate")]
    pub password_stdin: bool,
    /// Set a generated password and print it once.
    #[arg(long)]
    pub generate: bool,
    /// The user must choose a new password at the next sign-in.
    #[arg(long, conflicts_with = "no_reset_required")]
    pub reset_required: bool,
    /// The user need not choose a new password at the next sign-in.
    #[arg(long)]
    pub no_reset_required: bool,
}

/// `bb iam login-profile delete`
#[derive(Debug, Args)]
pub struct IamLoginDeleteArgs {
    /// The IAM user name.
    #[arg(value_name = "USER")]
    pub name: String,
    /// Confirm the deletion.
    #[arg(long)]
    pub yes: bool,
}

/// `bb iam validate-policy` and `bb iam format-policy`
#[derive(Debug, Args)]
pub struct IamPolicyFileArgs {
    /// The policy document; `-` reads standard input.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,
}

/// `bb ls`
#[derive(Debug, Args)]
pub struct LsArgs {
    /// `s3://bucket` or `s3://bucket/prefix`; omit to list buckets.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: Option<S3Uri>,
    /// List every object under the prefix instead of one folder level.
    #[arg(short, long, requires = "uri")]
    pub recursive: bool,
    /// List object versions and delete markers (always in the long format).
    #[arg(long, requires = "uri")]
    pub versions: bool,
    /// Show modification time, size and storage class (creation date and region for buckets).
    #[arg(short, long)]
    pub long: bool,
}

/// `bb mb`
#[derive(Debug, Args)]
pub struct MbArgs {
    /// Bucket name, or `s3://bucket`.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// Region to create the bucket in (default: the account region).
    #[arg(long, value_name = "REGION")]
    pub region: Option<String>,
}

/// `bb rb`
#[derive(Debug, Args)]
pub struct RbArgs {
    /// Bucket name, or `s3://bucket`. The bucket must be empty.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
}

/// `bb rm`
#[derive(Debug, Args)]
pub struct RmArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` (or a whole bucket) with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Delete everything under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Delete every version and delete marker, not just the current version.
    #[arg(long)]
    pub versions: bool,
    /// Delete this one version or delete marker of the object for good.
    #[arg(
        long,
        value_name = "ID",
        conflicts_with_all = ["recursive", "versions"]
    )]
    pub version_id: Option<String>,
    /// Print what would be deleted without deleting anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `bb restore-version`
#[derive(Debug, Args)]
pub struct RestoreVersionArgs {
    /// `s3://bucket/key` of the object.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// The version to make current (the IDs are in `bb ls --versions`).
    #[arg(long, value_name = "ID")]
    pub version_id: String,
}

/// `bb cp` and `bb mv`
#[derive(Debug, Args)]
pub struct CopyArgs {
    /// Source: a local file or folder, `s3://bucket/key`, or a prefix ending in `/`.
    #[arg(value_name = "SOURCE", value_parser = parse_location)]
    pub source: Location,
    /// Destination: a local path or folder, an exact key, or a prefix (ending in `/`, or a bucket).
    #[arg(value_name = "DESTINATION", value_parser = parse_location)]
    pub destination: Location,
    /// Copy a whole folder or prefix; the selected folder lands under the destination.
    #[arg(short, long)]
    pub recursive: bool,
    /// Include paths matching GLOB again (after an --exclude); later rules win.
    #[arg(long, value_name = "GLOB", requires = "recursive", value_parser = parse_glob)]
    pub include: Vec<String>,
    /// Skip paths matching GLOB (relative to the source folder or prefix); later rules win.
    #[arg(long, value_name = "GLOB", requires = "recursive", value_parser = parse_glob)]
    pub exclude: Vec<String>,
    /// Print what would be copied without changing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// What to do when the destination exists: overwrite, skip, newer (replace only when the
    /// source is newer) or rename (`name (1).ext`). Without it the transfer setting applies;
    /// `ask` asks for each conflict on a terminal and is refused elsewhere.
    #[arg(long, value_name = "POLICY", value_parser = parse_if_exists)]
    pub if_exists: Option<IfExists>,
    /// Do not store the local modification time of uploads, nor give downloaded files the time of the object.
    #[arg(long)]
    pub no_preserve_mtime: bool,
    /// Account that owns the source (a label or ID; default: --account).
    #[arg(long, value_name = "ACCOUNT")]
    pub from_account: Option<String>,
    /// Account that owns the destination bucket, for a copy or move between S3 locations.
    #[arg(long, value_name = "ACCOUNT")]
    pub to_account: Option<String>,
    /// Client-side encryption of uploads and decryption of downloads.
    #[command(flatten)]
    pub cse: CseArgs,
}

/// Client-side encryption options of `bb cp` and `bb mv`. The password is
/// never an argument: see `--password-file`.
#[derive(Debug, Args, Default)]
pub struct CseArgs {
    /// Encrypt uploaded files with a password before they leave this machine.
    #[arg(long, conflicts_with = "decrypt")]
    pub encrypt: bool,
    /// Decrypt downloaded objects that are client-side encrypted (others are saved as they are).
    #[arg(long)]
    pub decrypt: bool,
    /// Compress before encrypting: none, gzip or zstd.
    #[arg(long, value_name = "ALGORITHM", requires = "encrypt", value_parser = parse_compression)]
    pub compress: Option<Compression>,
    /// Read the password from the first line of FILE (else BB_CSE_PASSWORD, else asked on the terminal).
    #[arg(long, value_name = "FILE")]
    pub password_file: Option<PathBuf>,
}

/// `bb encrypt`
#[derive(Debug, Args)]
pub struct EncryptArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Encrypt every object under the prefix (objects that are encrypted already are skipped).
    #[arg(short, long)]
    pub recursive: bool,
    /// Compress before encrypting: none, gzip or zstd.
    #[arg(long, value_name = "ALGORITHM", value_parser = parse_compression)]
    pub compress: Option<Compression>,
    /// Read the password from the first line of FILE (else BB_CSE_PASSWORD, else asked on the terminal).
    #[arg(long, value_name = "FILE")]
    pub password_file: Option<PathBuf>,
}

/// `bb decrypt`
#[derive(Debug, Args)]
pub struct DecryptArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Decrypt every object under the prefix (objects that are not encrypted are skipped).
    #[arg(short, long)]
    pub recursive: bool,
    /// Read the password from the first line of FILE (else BB_CSE_PASSWORD, else asked on the terminal).
    #[arg(long, value_name = "FILE")]
    pub password_file: Option<PathBuf>,
}

fn parse_if_exists(text: &str) -> Result<IfExists, String> {
    text.parse::<IfExists>().map_err(|err| err.to_string())
}

fn parse_compression(text: &str) -> Result<Compression, String> {
    match text {
        "none" => Ok(Compression::None),
        "gzip" => Ok(Compression::Gzip),
        "zstd" => Ok(Compression::Zstd),
        other => Err(format!(
            "unknown compression '{other}' (none, gzip or zstd)"
        )),
    }
}

/// Which way `bb sync` copies files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SyncDirectionArg {
    /// Make the S3 prefix match the local folder (the default).
    Upload,
    /// Make the local folder match the S3 prefix.
    Download,
    /// Copy the newer file of each pair to the other side; nothing is ever deleted.
    Bidirectional,
}

/// The first location of `bb sync`: a local folder, or an S3 location for a
/// bucket-to-bucket sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncSource {
    /// A local folder.
    Local(PathBuf),
    /// An S3 prefix (the source of an S3 to S3 sync).
    S3(S3Uri),
}

fn parse_sync_source(text: &str) -> Result<SyncSource, String> {
    match parse_location(text)? {
        Location::Local(path) => Ok(SyncSource::Local(path)),
        Location::S3(uri) => Ok(SyncSource::S3(uri)),
    }
}

/// `bb sync jobs ...`
#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    /// List, show and delete saved sync jobs (jobs are created in the desktop app or with --save-job).
    #[command(subcommand)]
    Jobs(SyncJobsCommand),
}

/// `bb sync jobs`
#[derive(Debug, Subcommand)]
pub enum SyncJobsCommand {
    /// List the saved sync jobs.
    List,
    /// Show one saved sync job.
    Show(SyncJobName),
    /// Delete a saved sync job (needs --yes).
    Delete(SyncJobDelete),
}

/// A job named by its name or ID.
#[derive(Debug, Args)]
pub struct SyncJobName {
    /// Name (any letter case) or ID of the job.
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// `bb sync jobs delete`
#[derive(Debug, Args)]
pub struct SyncJobDelete {
    /// Name (any letter case) or ID of the job.
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Confirm the deletion.
    #[arg(long)]
    pub yes: bool,
}

impl From<SyncDirectionArg> for SyncDirection {
    fn from(direction: SyncDirectionArg) -> Self {
        match direction {
            SyncDirectionArg::Upload => SyncDirection::Upload,
            SyncDirectionArg::Download => SyncDirection::Download,
            SyncDirectionArg::Bidirectional => SyncDirection::Bidirectional,
        }
    }
}

/// `bb sync`
#[derive(Debug, Args)]
#[command(
    override_usage = "bb sync [OPTIONS] <LOCAL> <S3_URI>\n       bb sync [OPTIONS] <S3_SOURCE> <S3_URI>\n       bb sync [OPTIONS] --job <NAME>\n       bb sync [OPTIONS] --resume <PLAN_ID>\n       bb sync jobs <list|show|delete>",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct SyncArgs {
    /// Saved sync jobs.
    #[command(subcommand)]
    pub command: Option<SyncCommand>,
    /// Local folder, or an `s3://` location to sync from one S3 prefix to another. Always the first argument.
    #[arg(
        value_name = "LOCAL",
        value_parser = parse_sync_source,
        required_unless_present_any = ["resume", "job"],
        conflicts_with_all = ["resume", "job"]
    )]
    pub source: Option<SyncSource>,
    /// `s3://bucket` or `s3://bucket/prefix`, which names a folder. Always the second argument.
    #[arg(
        value_name = "S3_URI",
        value_parser = parse_sync_prefix,
        required_unless_present_any = ["resume", "job"],
        conflicts_with_all = ["resume", "job"]
    )]
    pub s3: Option<S3Uri>,
    /// Which way files are copied (default: upload; an S3 to S3 sync always copies from the first location to the second).
    #[arg(long, value_enum, value_name = "DIRECTION", conflicts_with_all = ["resume", "job"])]
    pub direction: Option<SyncDirectionArg>,
    /// Also delete what exists only on the target side (not with --direction bidirectional).
    #[arg(long, conflicts_with_all = ["resume", "job"])]
    pub delete: bool,
    /// Print the plan without changing anything: no file, object or stored plan is written.
    #[arg(long, conflicts_with = "resume")]
    pub dry_run: bool,
    /// Finish the interrupted plan with this ID; the folders, direction and --delete come from the plan.
    #[arg(long, value_name = "PLAN_ID", value_parser = parse_plan_id)]
    pub resume: Option<String>,
    /// Run the saved sync job with this name (or ID); it uses the job's account.
    #[arg(long, value_name = "NAME", conflicts_with = "resume")]
    pub job: Option<String>,
    /// Save this sync as a job with this name (replacing a job of that name), then run it.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["resume", "job"])]
    pub save_job: Option<String>,
    /// Gitignore-style rule that excludes paths (repeatable; `!` negates, a trailing `/` matches folders).
    #[arg(long, value_name = "RULE", conflicts_with = "resume")]
    pub exclude: Vec<String>,
    /// Sync only paths that match this glob (repeatable).
    #[arg(long, value_name = "GLOB", conflicts_with = "resume")]
    pub include: Vec<String>,
    /// Read exclusion rules from a file, one gitignore-style rule per line.
    #[arg(long, value_name = "FILE", conflicts_with = "resume")]
    pub exclude_from: Option<PathBuf>,
    /// Also sync empty folders (empty local folders get a folder marker in S3, and the other way round).
    #[arg(long, conflicts_with = "resume")]
    pub keep_empty_folders: bool,
    /// Treat files as equal when their size and checksum match, even if the modification times differ.
    #[arg(long, conflicts_with = "resume")]
    pub skip_duplicates: bool,
    /// Do not use the local cache of remote listings.
    #[arg(long, conflicts_with = "resume")]
    pub no_cache: bool,
    /// How many remote folders are listed at the same time (1 to 32).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=32), conflicts_with = "resume")]
    pub parallel: Option<u32>,
    /// Print a report at the end: counts and bytes per action, the duration and the failures (with --json, a final `report` record).
    #[arg(long)]
    pub summary: bool,
}

/// HTTP method of a presigned URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PresignMethod {
    /// Download the object.
    Get,
    /// Upload the object.
    Put,
}

impl PresignMethod {
    /// Lowercase name used in JSON records.
    pub fn as_str(self) -> &'static str {
        match self {
            PresignMethod::Get => "get",
            PresignMethod::Put => "put",
        }
    }
}

/// `bb edit`
#[derive(Debug, Args)]
pub struct EditArgs {
    /// `s3://bucket/key` of the object.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Open the copy with this application (an .app bundle on macOS, a program elsewhere) instead of the default one.
    #[arg(long, value_name = "APP")]
    pub app: Option<String>,
}

/// `bb presign`
#[derive(Debug, Args)]
pub struct PresignArgs {
    /// `s3://bucket/key` of the object.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// How long the URL is valid: 30s, 15m, 1h, 7d, 1h30m or seconds (1s to 7d).
    #[arg(long, value_name = "DURATION", default_value = "1h", value_parser = parse_expiry)]
    pub expires: Duration,
    /// Whether the URL downloads (get) or uploads (put) the object.
    #[arg(long, value_enum, default_value_t = PresignMethod::Get)]
    pub method: PresignMethod,
    /// Also send the URL to the URL shortener chosen in the settings and print the short link.
    /// The whole URL, signature included, leaves this machine.
    #[arg(long)]
    pub shorten: bool,
}

/// File format of `bb export`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormatArg {
    /// Comma-separated values with a header row.
    Csv,
    /// An Apache Parquet file.
    Parquet,
    /// An Excel workbook.
    Xlsx,
}

impl From<ExportFormatArg> for ExportFormat {
    fn from(format: ExportFormatArg) -> Self {
        match format {
            ExportFormatArg::Csv => ExportFormat::Csv,
            ExportFormatArg::Parquet => ExportFormat::Parquet,
            ExportFormatArg::Xlsx => ExportFormat::Xlsx,
        }
    }
}

/// `bb export`
#[derive(Debug, Args)]
pub struct ExportArgs {
    /// `s3://bucket` or `s3://bucket/prefix`; the prefix is listed like `bb ls` does.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// File format.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub format: ExportFormatArg,
    /// File to write; it is replaced when the export is complete.
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
    /// Every object below the prefix instead of one folder level.
    #[arg(short, long)]
    pub recursive: bool,
}

/// `bb mpu`
#[derive(Debug, Subcommand)]
pub enum MpuCommand {
    /// List uncompleted multipart uploads.
    List(MpuListArgs),
    /// Abort uncompleted multipart uploads (frees the storage of their parts).
    Abort(MpuAbortArgs),
}

/// `bb mount`
#[cfg(feature = "mount")]
#[derive(Debug, Args)]
pub struct MountArgs {
    /// Bucket to mount.
    #[arg(value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// Existing empty folder to mount it on (Windows: a free drive letter such as X: also works).
    #[arg(value_name = "DIR")]
    pub dir: std::path::PathBuf,
    /// Only this prefix of the bucket.
    #[arg(long, value_name = "PREFIX", default_value = "")]
    pub prefix: String,
}

/// `bb scan`
#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Only this bucket (default: every bucket of the account).
    #[arg(long, value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: Option<String>,
}

/// `bb mpu list`
#[derive(Debug, Args)]
pub struct MpuListArgs {
    /// Only this bucket (default: every bucket).
    #[arg(long, value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: Option<String>,
    /// Only uploads whose key starts with PREFIX.
    #[arg(long, value_name = "PREFIX", default_value = "")]
    pub prefix: String,
}

/// `bb mpu abort`
#[derive(Debug, Args)]
#[command(group(ArgGroup::new("target").required(true).args(["key", "all"])))]
pub struct MpuAbortArgs {
    /// Bucket of the uploads.
    #[arg(long, value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// Key of the upload to abort (with --upload-id).
    #[arg(long, value_name = "KEY", requires = "upload_id")]
    pub key: Option<String>,
    /// ID of the upload to abort (with --key).
    #[arg(long, value_name = "ID", requires = "key")]
    pub upload_id: Option<String>,
    /// Abort every uncompleted upload in the bucket (or under --prefix).
    #[arg(long)]
    pub all: bool,
    /// With --all: only uploads whose key starts with PREFIX.
    #[arg(long, value_name = "PREFIX", conflicts_with = "key")]
    pub prefix: Option<String>,
    /// Print what would be aborted without aborting anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `bb account`
#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Store a new account. The secret access key is never a command-line
    /// argument: it is read from standard input (--secret-stdin), from
    /// BB_SECRET_ACCESS_KEY, or asked on the terminal.
    Add(Box<AccountAddArgs>),
    /// List the stored accounts (`*` marks the default).
    List,
    /// Remove a stored account and its secrets.
    Remove(AccountSelector),
    /// Make a stored account the default.
    Default(AccountSelector),
    /// Import profiles of the AWS config and credentials files as accounts
    /// that stay linked to the files (no key is copied). Name profiles with
    /// --profile; without it every usable profile is imported.
    ImportAws,
    /// Sign in to AWS IAM Identity Center (SSO) with the device
    /// authorization flow; the token goes to the AWS CLI cache.
    SsoLogin(AccountSsoLoginArgs),
}

/// `bb account sso-login`
#[derive(Debug, Args)]
pub struct AccountSsoLoginArgs {
    /// Label or ID of a stored SSO account whose portal to sign in to.
    #[arg(value_name = "LABEL|ID")]
    pub account_label: Option<String>,
    /// Start URL of the portal (https://<directory>.awsapps.com/start).
    #[arg(long, value_name = "URL")]
    pub start_url: Option<String>,
    /// Region IAM Identity Center runs in.
    #[arg(long, value_name = "REGION")]
    pub sso_region: Option<String>,
    /// Name of the sso-session the token is cached under (as in the AWS config).
    #[arg(long, value_name = "NAME")]
    pub sso_session: Option<String>,
    /// Only print the URL and code; do not open the browser.
    #[arg(long)]
    pub no_browser: bool,
}

/// `bb account add`
#[derive(Debug, Args)]
pub struct AccountAddArgs {
    /// Display label, unique across accounts (case-insensitive).
    #[arg(long, value_name = "LABEL")]
    pub label: String,
    /// Access key ID (not a secret). Not used with --assume-role, --sso or --instance-role.
    #[arg(
        long,
        value_name = "ID",
        required_unless_present_any = ["assume_role", "sso", "instance_role"]
    )]
    pub access_key_id: Option<String>,
    /// Use the role of this EC2 instance or ECS/EKS container instead of keys.
    #[arg(long, conflicts_with_all = ["access_key_id", "assume_role", "sso", "secret_stdin"])]
    pub instance_role: bool,
    /// With --instance-role: add the account without checking that the role exists.
    #[arg(long, requires = "instance_role")]
    pub no_probe: bool,
    /// ARN of a role to assume through --source-account.
    #[arg(
        long,
        value_name = "ARN",
        requires = "source_account",
        conflicts_with_all = ["access_key_id", "sso", "instance_role", "secret_stdin"]
    )]
    pub assume_role: Option<String>,
    /// Label or ID of the stored account that signs the AssumeRole call.
    #[arg(long, value_name = "LABEL|ID", requires = "assume_role")]
    pub source_account: Option<String>,
    /// Role session name.
    #[arg(long, value_name = "NAME", requires = "assume_role")]
    pub session_name: Option<String>,
    /// Role session length (15m to 12h; seconds or 30m, 1h, ...).
    #[arg(
        long,
        value_name = "DURATION",
        requires = "assume_role",
        value_parser = crate::duration::parse_duration
    )]
    pub duration: Option<std::time::Duration>,
    /// External id the role's trust policy asks for.
    #[arg(long, value_name = "ID", requires = "assume_role")]
    pub external_id: Option<String>,
    /// Use an AWS IAM Identity Center (SSO) role (needs the --sso-* options).
    #[arg(
        long,
        requires_all = ["sso_start_url", "sso_region", "sso_account_id", "sso_role_name"],
        conflicts_with_all = ["access_key_id", "assume_role", "instance_role", "secret_stdin"]
    )]
    pub sso: bool,
    /// SSO portal start URL.
    #[arg(long, value_name = "URL", requires = "sso")]
    pub sso_start_url: Option<String>,
    /// SSO region.
    #[arg(long, value_name = "REGION", requires = "sso")]
    pub sso_region: Option<String>,
    /// AWS account id of the SSO role.
    #[arg(long, value_name = "ID", requires = "sso")]
    pub sso_account_id: Option<String>,
    /// Permission set (role) name of the SSO role.
    #[arg(long, value_name = "NAME", requires = "sso")]
    pub sso_role_name: Option<String>,
    /// Name of the sso-session the token is cached under.
    #[arg(long, value_name = "NAME", requires = "sso")]
    pub sso_session: Option<String>,
    /// Signing region.
    #[arg(long, value_name = "REGION", default_value = DEFAULT_REGION)]
    pub region: String,
    /// Custom endpoint URL for S3-compatible providers.
    #[arg(long, value_name = "URL")]
    pub endpoint: Option<String>,
    /// Use path-style addressing.
    #[arg(long)]
    pub path_style: bool,
    /// Pay for requests to requester-pays buckets.
    #[arg(long)]
    pub request_payer: bool,
    /// Serial number or ARN of the account's MFA device (for --assume-role:
    /// the device the role's trust policy asks for).
    #[arg(long, value_name = "SERIAL")]
    pub mfa_serial: Option<String>,
    /// Read the secret access key from the first line of standard input (a
    /// second line, if present, is a session token).
    #[arg(long)]
    pub secret_stdin: bool,
}

/// An account named by label (case-insensitive) or ID.
#[derive(Debug, Args)]
pub struct AccountSelector {
    /// Label or ID of the account. (Not called `account`: that is the ID of
    /// the global `--account`, and clap shares IDs with subcommands.)
    #[arg(value_name = "LABEL|ID")]
    pub label_or_id: String,
}

/// `bb tag`
#[derive(Debug, Subcommand)]
pub enum TagCommand {
    /// Print the tags of an object, or of every object under a prefix.
    Get(TagGetArgs),
    /// Add tags to objects; with --replace make them the whole tag set.
    Set(TagSetArgs),
    /// Remove the named tags from objects, or every tag when none is named.
    Delete(TagDeleteArgs),
}

/// `bb tag get`
#[derive(Debug, Args)]
pub struct TagGetArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Read the tags of every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
}

/// `bb tag set`
#[derive(Debug, Args)]
pub struct TagSetArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// The tags to set, each `KEY=VALUE`. Existing tags with other keys stay.
    #[arg(value_name = "KEY=VALUE", required = true, value_parser = parse_tag)]
    pub tags: Vec<(String, String)>,
    /// Make these tags the whole tag set, removing every other tag.
    #[arg(long)]
    pub replace: bool,
    /// Tag every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Print what would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `bb tag delete`
#[derive(Debug, Args)]
pub struct TagDeleteArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Keys of the tags to remove; without any, every tag is removed.
    #[arg(value_name = "KEY")]
    pub keys: Vec<String>,
    /// Remove tags from every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Print what would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `bb storage-class`
#[derive(Debug, Args)]
pub struct StorageClassArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// STANDARD, STANDARD_IA, ONEZONE_IA, GLACIER_IR, GLACIER, DEEP_ARCHIVE,
    /// INTELLIGENT_TIERING or REDUCED_REDUNDANCY.
    #[arg(value_name = "CLASS", value_parser = parse_class)]
    pub class: StorageClass,
    /// Change every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Print what would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// File with the SSE-C key of objects that are encrypted with a customer key
    /// (32 raw bytes, 64 hex digits or base64).
    #[arg(long, value_name = "FILE")]
    pub key_file: Option<PathBuf>,
}

/// The encryption `bb sse` changes objects to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseMode {
    /// SSE-S3.
    S3,
    /// SSE-KMS with an optional key ID, ARN or alias.
    Kms(Option<String>),
    /// SSE-C with the key of `--key-file`.
    Customer,
}

/// `bb sse`
#[derive(Debug, Args)]
pub struct SseArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// `s3` (SSE-S3), `kms` or `kms:KEYID` (SSE-KMS; KEYID is a key ID, ARN or
    /// alias, without it the AWS managed key) or `c` (SSE-C, needs --key-file).
    #[arg(value_name = "s3|kms[:KEYID]|c", value_parser = parse_sse_mode)]
    pub mode: SseMode,
    /// File with the new SSE-C key (32 raw bytes, 64 hex digits or base64).
    #[arg(long, value_name = "FILE")]
    pub key_file: Option<PathBuf>,
    /// File with the SSE-C key the objects have now, for objects that are
    /// encrypted with a customer key.
    #[arg(long, value_name = "FILE")]
    pub source_key_file: Option<PathBuf>,
    /// With kms: use an S3 Bucket Key.
    #[arg(long)]
    pub bucket_key: bool,
    /// Change every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Print what would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `bb acl`
#[derive(Debug, Subcommand)]
pub enum AclCommand {
    /// Print the ACL of an object, or of every object under a prefix.
    Get(AclGetArgs),
    /// Apply a canned ACL to objects.
    Set(AclSetArgs),
}

/// `bb acl get`
#[derive(Debug, Args)]
pub struct AclGetArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Read the ACL of every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
}

/// `bb acl set`
#[derive(Debug, Args)]
pub struct AclSetArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// private, public-read, public-read-write, authenticated-read,
    /// bucket-owner-read or bucket-owner-full-control.
    #[arg(value_name = "CANNED", value_parser = parse_acl)]
    pub acl: CannedAcl,
    /// Apply it to every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Print what would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `bb cloudfront`
#[derive(Debug, Subcommand)]
pub enum CloudfrontCommand {
    /// List the distributions of the account, or those that serve one bucket.
    List(CloudfrontListArgs),
    /// Show one distribution with its origins and aliases.
    Show(CloudfrontIdArgs),
    /// Create a distribution for a bucket (the bucket policy to add is printed).
    Create(CloudfrontCreateArgs),
    /// Let a distribution accept requests.
    Enable(CloudfrontIdArgs),
    /// Stop a distribution from accepting requests.
    Disable(CloudfrontIdArgs),
    /// Stop a distribution from serving a bucket (its origins are removed; nothing is deleted).
    Detach(CloudfrontDetachArgs),
    /// Invalidate cached paths (each starts with `/`; `*` only as the last character).
    Invalidate(CloudfrontInvalidateArgs),
    /// List the invalidation history of a distribution, newest first.
    Invalidations(CloudfrontIdArgs),
    /// Show one invalidation with its paths.
    Invalidation(CloudfrontInvalidationArgs),
}

/// `bb cloudfront list`
#[derive(Debug, Args)]
pub struct CloudfrontListArgs {
    /// Only the distributions that have an origin in this bucket.
    #[arg(long, value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: Option<String>,
}

/// A distribution ID.
#[derive(Debug, Args)]
pub struct CloudfrontIdArgs {
    /// Distribution ID, such as `E2ABCDEFGHIJKL`.
    #[arg(value_name = "ID")]
    pub id: String,
}

/// `bb cloudfront create`
#[derive(Debug, Args)]
pub struct CloudfrontCreateArgs {
    /// The bucket to serve.
    #[arg(long, value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
    /// Region of the bucket (default: the region `bb ls` reports for it).
    #[arg(long, value_name = "REGION")]
    pub region: Option<String>,
    /// Comment of the distribution (at most 128 characters).
    #[arg(long, value_name = "TEXT", default_value = "")]
    pub comment: String,
    /// Object served for the root URL, without a leading `/`.
    #[arg(long, value_name = "KEY", default_value = "")]
    pub default_root_object: String,
    /// `PriceClass_All` (default), `PriceClass_200` or `PriceClass_100`.
    #[arg(long, value_name = "CLASS")]
    pub price_class: Option<String>,
    /// Create the distribution disabled.
    #[arg(long)]
    pub disabled: bool,
}

/// `bb cloudfront detach`
#[derive(Debug, Args)]
pub struct CloudfrontDetachArgs {
    /// Distribution ID.
    #[arg(value_name = "ID")]
    pub id: String,
    /// The bucket whose origins are removed.
    #[arg(long, value_name = "BUCKET", value_parser = parse_bucket)]
    pub bucket: String,
}

/// `bb cloudfront invalidate`
#[derive(Debug, Args)]
pub struct CloudfrontInvalidateArgs {
    /// Distribution ID.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Paths to invalidate, such as `/index.html` or `/images/*`.
    #[arg(value_name = "PATH", required = true, num_args = 1..)]
    pub paths: Vec<String>,
}

/// `bb cloudfront invalidation`
#[derive(Debug, Args)]
pub struct CloudfrontInvalidationArgs {
    /// Distribution ID.
    #[arg(value_name = "ID")]
    pub id: String,
    /// Invalidation ID.
    #[arg(value_name = "INVALIDATION_ID")]
    pub invalidation_id: String,
}

/// `bb lock`
#[derive(Debug, Subcommand)]
pub enum LockCommand {
    /// Print the retention and legal hold of an object, or of every object under a prefix.
    Get(LockGetArgs),
    /// Set or clear the retention and the legal hold of objects.
    Set(LockSetArgs),
}

/// `bb lock get`
#[derive(Debug, Args)]
pub struct LockGetArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Read every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Read this version of the object.
    #[arg(long, value_name = "ID", conflicts_with = "recursive")]
    pub version_id: Option<String>,
}

/// A legal hold setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HoldState {
    /// Put a legal hold on the objects.
    On,
    /// Lift the legal hold.
    Off,
}

/// `bb lock set`
#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("change")
        .required(true)
        .multiple(true)
        .args(["mode", "clear_retention", "legal_hold"])
))]
pub struct LockSetArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Retention mode: GOVERNANCE or COMPLIANCE (needs --until).
    #[arg(long, value_name = "MODE", value_parser = parse_lock_mode, requires = "until")]
    pub mode: Option<LockMode>,
    /// End of the retention: an RFC 3339 time or a date (YYYY-MM-DD, UTC midnight).
    #[arg(long, value_name = "DATE", value_parser = parse_until, requires = "mode")]
    pub until: Option<DateTime<Utc>>,
    /// Remove the retention (a governance retention needs --bypass-governance).
    #[arg(long, conflicts_with_all = ["mode", "until"])]
    pub clear_retention: bool,
    /// Turn the legal hold on or off.
    #[arg(long, value_name = "on|off", value_enum)]
    pub legal_hold: Option<HoldState>,
    /// Let a user with s3:BypassGovernanceRetention shorten or remove a governance retention.
    #[arg(long)]
    pub bypass_governance: bool,
    /// Change every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Change this version of the object.
    #[arg(long, value_name = "ID", conflicts_with = "recursive")]
    pub version_id: Option<String>,
    /// Print what would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
}

/// `bb restore`
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// `s3://bucket/key`, or a prefix ending in `/` with --recursive.
    #[arg(value_name = "S3_URI", value_parser = parse_s3_uri)]
    pub uri: S3Uri,
    /// Days the restored copy stays available.
    #[arg(long, value_name = "N")]
    pub days: u32,
    /// Expedited, Standard or Bulk.
    #[arg(long, value_name = "TIER", default_value = "Standard", value_parser = parse_tier)]
    pub tier: RestoreTier,
    /// Restore every object under the prefix.
    #[arg(short, long)]
    pub recursive: bool,
    /// Restore this version of the object.
    #[arg(long, value_name = "ID", conflicts_with = "recursive")]
    pub version_id: Option<String>,
    /// Print what would be requested without requesting anything.
    #[arg(long)]
    pub dry_run: bool,
}

fn parse_lock_mode(text: &str) -> Result<LockMode, String> {
    objectlock::parse_mode(text).map_err(|err| err.to_string())
}

fn parse_tier(text: &str) -> Result<RestoreTier, String> {
    text.parse().map_err(|err: Error| err.to_string())
}

/// An RFC 3339 time, or a date taken as midnight UTC.
fn parse_until(text: &str) -> Result<DateTime<Utc>, String> {
    if let Ok(time) = DateTime::parse_from_rfc3339(text.trim()) {
        return Ok(time.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc())
        .ok_or_else(|| format!("'{text}' is not a date; use YYYY-MM-DD or an RFC 3339 time"))
}

/// `KEY=VALUE`, checked against the S3 tag rules.
fn parse_tag(text: &str) -> Result<(String, String), String> {
    let (key, value) = text
        .split_once('=')
        .ok_or_else(|| format!("'{text}' is not KEY=VALUE"))?;
    validate_tag(key, value).map_err(|err| err.to_string())?;
    Ok((key.to_owned(), value.to_owned()))
}

fn parse_class(text: &str) -> Result<StorageClass, String> {
    text.parse().map_err(|err: Error| err.to_string())
}

fn parse_acl(text: &str) -> Result<CannedAcl, String> {
    text.parse().map_err(|err: Error| err.to_string())
}

/// `s3`, `c`, `kms` or `kms:KEYID`; the key ID keeps its case.
fn parse_sse_mode(text: &str) -> Result<SseMode, String> {
    let (name, key) = match text.split_once(':') {
        Some((name, key)) => (name, Some(key)),
        None => (text, None),
    };
    match (name.to_ascii_lowercase().as_str(), key) {
        ("s3", None) => Ok(SseMode::S3),
        ("c", None) => Ok(SseMode::Customer),
        ("kms", None) => Ok(SseMode::Kms(None)),
        ("kms", Some(key)) if !key.trim().is_empty() => Ok(SseMode::Kms(Some(key.to_owned()))),
        ("kms", Some(_)) => Err("kms: needs a key ID, ARN or alias after the colon".to_owned()),
        _ => Err(format!(
            "'{text}' is not an encryption; use s3, kms, kms:KEYID or c"
        )),
    }
}

fn parse_glob(text: &str) -> Result<String, String> {
    Glob::new(text)
        .map(|_| text.to_owned())
        .map_err(|err| err.to_string())
}

/// The S3 side of `bb sync`: an S3 URI (a prefix names a folder).
fn parse_sync_prefix(text: &str) -> Result<S3Uri, String> {
    parse_s3_uri(text).map_err(|err| {
        format!(
            "{err} (bb sync takes the local folder first and the S3 location second; two S3 locations sync from the first to the second)"
        )
    })
}

/// A plan ID as `bb sync` prints it (a UUID, 8-4-4-4-12 hexadecimal digits);
/// the plan store checks the rest. Spelled in lowercase, as stored.
fn parse_plan_id(text: &str) -> Result<String, String> {
    let well_formed = text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        });
    if well_formed {
        Ok(text.to_ascii_lowercase())
    } else {
        Err(format!(
            "'{text}' is not a plan ID (expected a UUID such as 5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35)"
        ))
    }
}

/// The `--include` / `--exclude` rules of a `cp` or `mv` invocation, in
/// command-line order (clap keeps the two flags apart; their positions
/// restore the order).
pub fn copy_rules(matches: &ArgMatches) -> Result<IncludeExclude, Error> {
    let Some(("cp" | "mv", sub)) = matches.subcommand() else {
        return Ok(IncludeExclude::default());
    };
    let positioned = |id: &str| -> Vec<(usize, String)> {
        match (sub.get_many::<String>(id), sub.indices_of(id)) {
            (Some(values), Some(indices)) => indices.zip(values.cloned()).collect(),
            _ => Vec::new(),
        }
    };
    IncludeExclude::from_positions(positioned("include"), positioned("exclude"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};

    fn parse(args: &[&str]) -> Result<(Cli, ArgMatches), clap::Error> {
        let matches = Cli::command()
            .try_get_matches_from(std::iter::once("bb").chain(args.iter().copied()))?;
        let cli = Cli::from_arg_matches(&matches)?;
        Ok((cli, matches))
    }

    #[test]
    fn export_needs_a_format_and_a_file() {
        let (cli, _) = parse(&[
            "export",
            "s3://b/photos/",
            "--format",
            "parquet",
            "--out",
            "listing.parquet",
            "-r",
        ])
        .unwrap();
        let Command::Export(args) = cli.command else {
            panic!("export expected")
        };
        assert_eq!(args.uri.to_string(), "s3://b/photos/");
        assert_eq!(ExportFormat::from(args.format), ExportFormat::Parquet);
        assert_eq!(args.out, PathBuf::from("listing.parquet"));
        assert!(args.recursive);
        assert!(Command::Export(args).is_transfer());
        for args in [
            vec!["export", "s3://b/", "--out", "x.csv"],
            vec!["export", "s3://b/", "--format", "csv"],
            vec!["export", "s3://b/", "--format", "json", "--out", "x"],
            vec!["export", "--format", "csv", "--out", "x.csv"],
            vec!["export", "plain", "--format", "csv", "--out", "x.csv"],
        ] {
            assert!(parse(&args).is_err(), "{args:?} should be rejected");
        }
    }

    #[test]
    fn the_account_argument_is_not_the_global_account() {
        for args in [
            &["account", "default", "HOME"][..],
            &["account", "remove", "HOME"],
        ] {
            let (cli, _) = parse(args).unwrap();
            assert_eq!(cli.global.account, None, "{args:?}");
            let (Command::Account(AccountCommand::Default(selector))
            | Command::Account(AccountCommand::Remove(selector))) = cli.command
            else {
                panic!("account default or remove expected")
            };
            assert_eq!(selector.label_or_id, "HOME");
        }
        let (cli, _) = parse(&["--account", "work", "account", "list"]).unwrap();
        assert_eq!(cli.global.account.as_deref(), Some("work"));
    }

    #[test]
    fn definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn rules_keep_command_line_order_across_both_flags() {
        let (_, matches) = parse(&[
            "cp",
            "-r",
            "s3://a/p/",
            "s3://b/",
            "--exclude",
            "*",
            "--include",
            "*.jpg",
            "--exclude",
            "secret*",
        ])
        .unwrap();
        let rules = copy_rules(&matches).unwrap();
        assert!(rules.is_included("a.jpg"));
        assert!(!rules.is_included("a.png"));
        assert!(!rules.is_included("secret.jpg"));

        let (_, matches) = parse(&[
            "mv",
            "-r",
            "./dir",
            "s3://b/",
            "--include",
            "*.jpg",
            "--exclude",
            "*",
        ])
        .unwrap();
        assert!(!copy_rules(&matches).unwrap().is_included("a.jpg"));

        let (_, matches) = parse(&["ls"]).unwrap();
        assert!(copy_rules(&matches).unwrap().is_empty());
    }

    #[test]
    fn global_flags_work_before_and_after_the_command() {
        let (cli, _) = parse(&[
            "--json",
            "ls",
            "--quiet",
            "--account",
            "work",
            "--mfa-code",
            "123456",
        ])
        .unwrap();
        assert!(cli.global.json && cli.global.quiet);
        assert_eq!(cli.global.account.as_deref(), Some("work"));
        assert_eq!(cli.global.mfa_code.as_deref(), Some("123456"));
        let (cli, _) = parse(&["ls", "--config-dir", "/tmp/x"]).unwrap();
        assert_eq!(cli.global.config_dir, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn locations_are_local_or_s3() {
        let (cli, _) = parse(&["cp", "./a.txt", "s3://b/k"]).unwrap();
        let Command::Cp(args) = cli.command else {
            panic!("cp expected")
        };
        assert_eq!(args.source, Location::Local(PathBuf::from("./a.txt")));
        assert!(matches!(args.destination, Location::S3(_)));
        assert!(Command::Cp(args).is_transfer());
    }

    #[test]
    fn secrets_are_never_arguments() {
        for args in [
            vec![
                "account",
                "add",
                "--label",
                "w",
                "--access-key-id",
                "AK",
                "--secret",
                "s",
            ],
            vec![
                "account",
                "add",
                "--label",
                "w",
                "--access-key-id",
                "AK",
                "--secret-access-key",
                "s",
            ],
            vec![
                "account",
                "add",
                "--label",
                "w",
                "--access-key-id",
                "AK",
                "--session-token",
                "t",
            ],
            vec![
                "account",
                "add",
                "--label",
                "w",
                "--access-key-id",
                "AK",
                "secret",
            ],
        ] {
            assert!(parse(&args).is_err(), "{args:?} should be rejected");
        }
        let (cli, _) = parse(&[
            "account",
            "add",
            "--label",
            "w",
            "--access-key-id",
            "AK",
            "--secret-stdin",
        ])
        .unwrap();
        let Command::Account(AccountCommand::Add(args)) = cli.command else {
            panic!("account add expected")
        };
        assert!(args.secret_stdin);
        assert_eq!(args.region, "us-east-1");
    }

    #[test]
    fn invalid_combinations_are_usage_errors() {
        for args in [
            vec!["ls", "--recursive"],
            vec!["ls", "--versions"],
            vec!["restore-version", "s3://a/k"],
            vec!["restore-version", "--version-id", "v1"],
            vec!["rm", "s3://a/k", "--version-id", "v1", "--versions"],
            vec!["rm", "-r", "s3://a/k/", "--version-id", "v1"],
            vec!["rm", "s3://a/k", "--version-id"],
            vec!["cp", "s3://a/k", "s3://b/k", "--include", "*"],
            vec!["cp", "-r", "s3://a/p/", "s3://b/", "--exclude", ""],
            vec!["cp", "-r", "s3://a/p/", "s3://b/", "--exclude", "a[b"],
            vec!["cp", "s3:/broken", "./x"],
            vec!["presign", "s3://a/k", "--expires", "8d"],
            vec!["presign", "s3://a/k", "--method", "delete"],
            vec!["mb", "s3://a/key"],
            vec!["mpu", "abort", "--bucket", "b"],
            vec!["mpu", "abort", "--bucket", "b", "--key", "k"],
            vec![
                "mpu",
                "abort",
                "--bucket",
                "b",
                "--all",
                "--key",
                "k",
                "--upload-id",
                "u",
            ],
            vec![
                "mpu",
                "abort",
                "--bucket",
                "b",
                "--key",
                "k",
                "--upload-id",
                "u",
                "--prefix",
                "p",
            ],
            vec!["mpu", "list", "--bucket", "s3://b/k"],
            vec!["account", "add", "--label", "w"],
            vec!["account", "remove"],
            vec!["sync"],
            vec!["sync", "dir"],
            vec!["sync", "s3://a/p", "dir"],
            vec!["sync", "dir", "plain"],
            vec!["sync", "dir", "s3://a/p", "--direction", "sideways"],
            vec!["sync", "--resume", "nope"],
            vec!["sync", "--resume", PLAN_ID, "dir", "s3://a/p"],
            vec!["sync", "--resume", PLAN_ID, "--delete"],
            vec!["sync", "--resume", PLAN_ID, "--dry-run"],
            vec!["sync", "--resume", PLAN_ID, "--direction", "upload"],
        ] {
            assert!(parse(&args).is_err(), "{args:?} should be rejected");
        }
    }

    const PLAN_ID: &str = "5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35";

    #[test]
    fn lock_and_restore_arguments() {
        let (cli, _) = parse(&[
            "lock",
            "set",
            "s3://b/k",
            "--mode",
            "governance",
            "--until",
            "2099-01-02",
            "--legal-hold",
            "on",
            "--bypass-governance",
        ])
        .unwrap();
        let Command::Lock(LockCommand::Set(args)) = cli.command else {
            panic!("lock set expected")
        };
        assert_eq!(args.mode, Some(LockMode::Governance));
        assert_eq!(
            args.until.unwrap().to_rfc3339(),
            "2099-01-02T00:00:00+00:00"
        );
        assert_eq!(args.legal_hold, Some(HoldState::On));
        assert!(args.bypass_governance && !args.clear_retention);
        for bad in [
            vec!["lock", "set", "s3://b/k"],
            vec!["lock", "set", "s3://b/k", "--mode", "governance"],
            vec!["lock", "set", "s3://b/k", "--until", "2099-01-02"],
            vec![
                "lock",
                "set",
                "s3://b/k",
                "--mode",
                "x",
                "--until",
                "2099-01-02",
            ],
            vec![
                "lock",
                "set",
                "s3://b/k",
                "--until",
                "soon",
                "--mode",
                "compliance",
            ],
            vec![
                "lock",
                "set",
                "s3://b/k",
                "--clear-retention",
                "--mode",
                "compliance",
                "--until",
                "2099-01-02",
            ],
            vec!["restore", "s3://b/k"],
            vec!["restore", "s3://b/k", "--days", "3", "--tier", "fast"],
        ] {
            assert!(parse(&bad).is_err(), "{bad:?} should be rejected");
        }
        let (cli, _) = parse(&["lock", "set", "s3://b/k", "--clear-retention"]).unwrap();
        assert!(matches!(cli.command, Command::Lock(LockCommand::Set(_))));
        let (cli, _) = parse(&["restore", "s3://b/k", "--days", "3", "--tier", "bulk"]).unwrap();
        let Command::Restore(args) = cli.command else {
            panic!("restore expected")
        };
        assert_eq!((args.days, args.tier), (3, RestoreTier::Bulk));
        let (cli, _) = parse(&["restore", "s3://b/k", "--days", "1"]).unwrap();
        let Command::Restore(args) = cli.command else {
            panic!("restore expected")
        };
        assert_eq!(args.tier, RestoreTier::Standard);
        let (cli, _) = parse(&["restore-version", "s3://b/k", "--version-id", "v"]).unwrap();
        assert!(matches!(cli.command, Command::RestoreVersion(_)));
    }

    #[test]
    fn cloudfront_subcommands() {
        let (cli, _) = parse(&["cloudfront", "list", "--bucket", "s3://site"]).unwrap();
        let Command::Cloudfront(CloudfrontCommand::List(args)) = cli.command else {
            panic!("cloudfront list expected")
        };
        assert_eq!(args.bucket.as_deref(), Some("site"));
        let (cli, _) = parse(&["cloudfront", "invalidate", "EA", "/a", "/b/*"]).unwrap();
        let Command::Cloudfront(CloudfrontCommand::Invalidate(args)) = cli.command else {
            panic!("cloudfront invalidate expected")
        };
        assert_eq!((args.id.as_str(), args.paths.len()), ("EA", 2));
        assert!(parse(&["cloudfront", "invalidate", "EA"]).is_err());
        assert!(parse(&["cloudfront", "create"]).is_err());
        assert!(parse(&["cloudfront", "detach", "EA"]).is_err());
        let (cli, _) = parse(&["cloudfront", "create", "--bucket", "b", "--disabled"]).unwrap();
        let Command::Cloudfront(CloudfrontCommand::Create(args)) = cli.command else {
            panic!("cloudfront create expected")
        };
        assert!(args.disabled && args.region.is_none());
    }

    #[test]
    fn versions_are_chosen_by_id() {
        let (cli, _) =
            parse(&["restore-version", "s3://b/dir/k.txt", "--version-id", "v1"]).unwrap();
        let Command::RestoreVersion(args) = cli.command else {
            panic!("restore-version expected")
        };
        assert_eq!(args.uri.to_string(), "s3://b/dir/k.txt");
        assert_eq!(args.version_id, "v1");
        let (cli, _) = parse(&["rm", "--dry-run", "s3://b/k", "--version-id", "3HL4kq+r"]).unwrap();
        let Command::Rm(args) = cli.command else {
            panic!("rm expected")
        };
        assert_eq!(args.version_id.as_deref(), Some("3HL4kq+r"));
        assert!(args.dry_run && !args.versions && !args.recursive);
    }

    #[test]
    fn sync_takes_the_local_folder_first() {
        let (cli, _) = parse(&["sync", "photos", "s3://b/backup", "--delete"]).unwrap();
        let Command::Sync(args) = cli.command else {
            panic!("sync expected")
        };
        assert_eq!(
            args.source,
            Some(SyncSource::Local(PathBuf::from("photos")))
        );
        assert_eq!(
            args.s3.map(|uri| uri.to_string()).as_deref(),
            Some("s3://b/backup")
        );
        assert_eq!(args.direction, None);
        assert!(args.delete && !args.dry_run && args.resume.is_none());

        let (cli, _) = parse(&[
            "sync",
            "--direction",
            "bidirectional",
            "--dry-run",
            ".",
            "s3://b",
        ])
        .unwrap();
        let Command::Sync(args) = cli.command else {
            panic!("sync expected")
        };
        assert_eq!(
            args.direction.map(SyncDirection::from),
            Some(SyncDirection::Bidirectional)
        );
        assert!(args.dry_run && !args.delete);
        assert!(Command::Sync(args).is_transfer());

        // A resume names only the plan, in either case.
        let (cli, _) = parse(&["sync", "--resume", &PLAN_ID.to_uppercase()]).unwrap();
        let Command::Sync(args) = cli.command else {
            panic!("sync expected")
        };
        assert_eq!(args.resume.as_deref(), Some(PLAN_ID));
        assert!(args.source.is_none() && args.s3.is_none());
    }

    #[test]
    fn sync_options_jobs_and_s3_to_s3_parse() {
        let sync = |args: &[&str]| {
            let (cli, _) = parse(&[&["sync"], args].concat()).unwrap();
            match cli.command {
                Command::Sync(args) => args,
                other => panic!("sync expected, got {other:?}"),
            }
        };
        let args = sync(&[
            "dir",
            "s3://b/p",
            "--exclude",
            "*.tmp",
            "--exclude",
            "build/",
            "--include",
            "**/*.rs",
            "--exclude-from",
            "rules.txt",
            "--keep-empty-folders",
            "--skip-duplicates",
            "--no-cache",
            "--parallel",
            "8",
            "--summary",
            "--save-job",
            "nightly",
        ]);
        assert_eq!(args.exclude, ["*.tmp", "build/"]);
        assert_eq!(args.include, ["**/*.rs"]);
        assert_eq!(args.exclude_from, Some(PathBuf::from("rules.txt")));
        assert!(args.keep_empty_folders && args.skip_duplicates && args.no_cache && args.summary);
        assert_eq!(args.parallel, Some(8));
        assert_eq!(args.save_job.as_deref(), Some("nightly"));

        // Two S3 locations are a sync between prefixes.
        let args = sync(&["s3://a/x", "s3://b/y"]);
        assert!(matches!(args.source, Some(SyncSource::S3(_))));

        // A job names everything; locations, direction and delete would contradict it.
        let args = sync(&["--job", "nightly", "--dry-run", "--summary"]);
        assert_eq!(args.job.as_deref(), Some("nightly"));
        assert!(args.source.is_none() && args.dry_run);
        for bad in [
            vec!["--job", "j", "dir", "s3://b"],
            vec!["--job", "j", "--delete"],
            vec!["--job", "j", "--direction", "upload"],
            vec!["--job", "j", "--resume", PLAN_ID],
            vec!["dir", "s3://b", "--parallel", "0"],
            vec!["dir", "s3://b", "--parallel", "33"],
        ] {
            assert!(parse(&[&["sync"], &bad[..]].concat()).is_err(), "{bad:?}");
        }

        // The jobs subcommands.
        let args = sync(&["jobs", "list"]);
        assert!(matches!(
            args.command,
            Some(SyncCommand::Jobs(SyncJobsCommand::List))
        ));
        let args = sync(&["jobs", "delete", "nightly", "--yes"]);
        let Some(SyncCommand::Jobs(SyncJobsCommand::Delete(delete))) = args.command else {
            panic!("jobs delete expected")
        };
        assert!(delete.yes && delete.name == "nightly");
        // Positional arguments and a subcommand do not mix.
        assert!(parse(&["sync", "dir", "s3://b", "jobs", "list"]).is_err());
    }

    #[test]
    fn plan_ids_are_uuids() {
        assert_eq!(parse_plan_id(PLAN_ID).unwrap(), PLAN_ID);
        for bad in [
            "",
            "nope",
            "5b0e6a7a3f1c4d2e9a470c8d2f6e1b35",
            "5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b3",
            "5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35x",
            "5b0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b3g",
            "../0e6a7a-3f1c-4d2e-9a47-0c8d2f6e1b35",
        ] {
            assert!(parse_plan_id(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn object_metadata_commands_parse() {
        let (cli, _) = parse(&[
            "tag",
            "set",
            "s3://b/k",
            "env=prod",
            "team=data",
            "--replace",
            "-r",
        ])
        .unwrap();
        let Command::Tag(TagCommand::Set(args)) = cli.command else {
            panic!("tag set expected")
        };
        assert_eq!(
            args.tags,
            [
                ("env".to_owned(), "prod".to_owned()),
                ("team".to_owned(), "data".to_owned())
            ]
        );
        assert!(args.replace && args.recursive && !args.dry_run);

        let (cli, _) = parse(&["tag", "delete", "s3://b/k"]).unwrap();
        let Command::Tag(TagCommand::Delete(args)) = cli.command else {
            panic!("tag delete expected")
        };
        assert!(args.keys.is_empty());

        let (cli, _) = parse(&[
            "storage-class",
            "s3://b/p/",
            "glacier-ir",
            "-r",
            "--dry-run",
        ])
        .unwrap();
        let Command::StorageClass(args) = cli.command else {
            panic!("storage-class expected")
        };
        assert_eq!(args.class, StorageClass::GlacierIr);
        assert!(args.recursive && args.dry_run && args.key_file.is_none());

        for (text, mode) in [
            ("s3", SseMode::S3),
            ("S3", SseMode::S3),
            ("c", SseMode::Customer),
            ("kms", SseMode::Kms(None)),
            (
                "KMS:Alias/Data",
                SseMode::Kms(Some("Alias/Data".to_owned())),
            ),
            (
                "kms:arn:aws:kms:eu-west-1:000000000000:key/abc",
                SseMode::Kms(Some(
                    "arn:aws:kms:eu-west-1:000000000000:key/abc".to_owned(),
                )),
            ),
        ] {
            let (cli, _) = parse(&["sse", "s3://b/k", text, "--bucket-key"]).unwrap();
            let Command::Sse(args) = cli.command else {
                panic!("sse expected")
            };
            assert_eq!(args.mode, mode, "{text}");
            assert!(args.bucket_key);
        }

        let (cli, _) = parse(&["acl", "set", "s3://b/k", "public_read"]).unwrap();
        let Command::Acl(AclCommand::Set(args)) = cli.command else {
            panic!("acl set expected")
        };
        assert_eq!(args.acl, CannedAcl::PublicRead);
        assert!(!Command::Acl(AclCommand::Set(args)).is_transfer());
    }

    #[test]
    fn invalid_object_metadata_arguments_are_rejected() {
        for args in [
            vec!["tag", "set", "s3://b/k"],
            vec!["tag", "set", "s3://b/k", "novalue"],
            vec!["tag", "set", "s3://b/k", "k=semi;colon"],
            vec!["tag", "get"],
            vec!["storage-class", "s3://b/k"],
            vec!["storage-class", "s3://b/k", "COLD"],
            vec!["sse", "s3://b/k", "kms:"],
            vec!["sse", "s3://b/k", "aes256"],
            vec!["acl", "set", "s3://b/k", "public"],
            vec!["acl", "get", "plain"],
        ] {
            assert!(parse(&args).is_err(), "{args:?} should be rejected");
        }
    }

    #[test]
    fn defaults() {
        let (cli, _) = parse(&["presign", "s3://a/k"]).unwrap();
        let Command::Presign(args) = cli.command else {
            panic!("presign expected")
        };
        assert_eq!(args.expires, Duration::from_secs(3600));
        assert_eq!(args.method, PresignMethod::Get);
        let (cli, _) = parse(&[
            "mpu", "abort", "--bucket", "s3://b", "--all", "--prefix", "tmp/",
        ])
        .unwrap();
        let Command::Mpu(MpuCommand::Abort(args)) = cli.command else {
            panic!("mpu abort expected")
        };
        assert_eq!(args.bucket, "b");
        assert_eq!(args.prefix.as_deref(), Some("tmp/"));
    }
}
