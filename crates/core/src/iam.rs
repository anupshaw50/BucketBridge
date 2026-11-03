//! IAM user management (task T02): users, their managed and inline
//! policies, access keys and console login profiles.
//!
//! [`Iam`] wraps an `aws-sdk-iam` client built like the STS client of
//! [`crate::mfa`]: the account's credentials (or its MFA session, see
//! [`Session::iam_for`](crate::session::Session::iam_for)), the proxy-aware
//! HTTP client, retries from the settings and the read timeout of the
//! settings on every call. IAM is a global AWS service, so an account with a
//! custom endpoint (any S3-compatible server) has no IAM here:
//! [`iam_supported`] says so and [`iam_config`] fails with
//! [`NOT_AVAILABLE`].
//!
//! # Lists
//!
//! Every list call follows the `Marker` of IAM until the answer is no longer
//! truncated. A marker that comes back twice (a server that loops) ends the
//! walk with an error instead of an endless loop, and so does a walk of more
//! than [`MAX_PAGES`] pages.
//!
//! # Secrets
//!
//! The secret of a new access key ([`Iam::create_access_key`]) is returned
//! once, inside a [`NewAccessKey`]: it is never stored, logged or persisted,
//! its `Debug` is redacted and it is zeroed when dropped. Login profile
//! passwords travel as [`SecretString`]s and are never logged either. The
//! only copy the core cannot clear is the one inside the SDK's own response
//! value, which is dropped as soon as the secret is moved out of it.
//!
//! # Destructive actions
//!
//! Deleting a user, an access key, an inline policy or a login profile takes
//! the typed confirmation of the dialog as an argument and refuses with
//! `usage` unless it equals the name (or access key ID) of the target, so no
//! caller can skip the confirmation.
//!
//! # Deleting a user
//!
//! IAM refuses to delete a user that still owns anything.
//! [`Iam::delete_user`] removes the access keys, the inline policies, the
//! attached managed policies, the login profile and the group memberships
//! first, one step at a time, and reports every step in a
//! [`DeleteUserOutcome`]. A step that fails does not stop the others (so
//! one run removes as much as it can), but the user itself is only deleted
//! when every step before it worked.

use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::time::Duration;

use aws_sdk_iam::config::retry::RetryConfig;
use aws_sdk_iam::config::timeout::TimeoutConfig;
use aws_sdk_iam::config::{BehaviorVersion, Region, SharedHttpClient};
use aws_sdk_iam::types::{PolicyScopeType, StatusType};
use chrono::{DateTime, Utc};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use zeroize::ZeroizeOnDrop;

use crate::bucketcfg::{PolicyIssue, Section};
use crate::client::{ClientOptions, Credentials, validate_region};
use crate::error::{BbError, Error, Result};
use crate::model::Account;
use crate::secrets::SecretString;

/// The message of every IAM operation on an account with a custom endpoint.
pub const NOT_AVAILABLE: &str = "IAM is not available for this endpoint";

/// Most pages one list call follows.
pub const MAX_PAGES: usize = 1000;

/// Items asked for per page.
const PAGE_SIZE: i32 = 1000;

/// Longest inline policy document IAM accepts for a user, without
/// whitespace.
pub const MAX_INLINE_POLICY_CHARS: usize = 2048;

const CREDENTIALS_PROVIDER_NAME: &str = "BucketBridge";

/// Whether IAM can be used with `account`: it is an AWS account (no custom
/// endpoint).
pub fn iam_supported(account: &Account) -> bool {
    account.endpoint.is_none()
}

/// IAM client configuration for `account` signed with `credentials`:
/// retries from `options`, the connect timeout of `options`, and the SDK's
/// own endpoint for the region (IAM's global endpoint). Callers add the
/// HTTP client before building. Fails with `usage` for an account with a
/// custom endpoint ([`NOT_AVAILABLE`]).
pub fn iam_config(
    account: &Account,
    credentials: &Credentials,
    options: &ClientOptions,
) -> Result<aws_sdk_iam::config::Builder> {
    let static_credentials = aws_sdk_iam::config::Credentials::new(
        credentials.access_key_id.as_str(),
        credentials.secret_access_key.as_str(),
        credentials.session_token.clone(),
        None,
        CREDENTIALS_PROVIDER_NAME,
    );
    iam_config_with_provider(
        account,
        aws_credential_types::provider::SharedCredentialsProvider::new(static_credentials),
        options,
    )
}

/// [`iam_config`] signed by a refreshing credentials provider (see
/// [`crate::creds`]).
pub fn iam_config_with_provider(
    account: &Account,
    provider: aws_credential_types::provider::SharedCredentialsProvider,
    options: &ClientOptions,
) -> Result<aws_sdk_iam::config::Builder> {
    if !iam_supported(account) {
        return Err(Error::Usage(NOT_AVAILABLE.to_owned()));
    }
    validate_region(&account.region)?;
    Ok(aws_sdk_iam::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(account.region.clone()))
        .credentials_provider(provider)
        .retry_config(RetryConfig::standard().with_max_attempts(options.max_attempts.max(1)))
        .timeout_config(
            TimeoutConfig::builder()
                .connect_timeout(options.connect_timeout)
                .build(),
        ))
}

/// Walks the pages of one list call. `fetch` gets the marker of the page to
/// read (`None` for the first) and returns its items and the next marker
/// (`None` on the last page).
async fn paged<T, F, Fut>(what: &str, mut fetch: F) -> Result<Vec<T>>
where
    F: FnMut(Option<String>) -> Fut,
    Fut: Future<Output = Result<(Vec<T>, Option<String>)>>,
{
    let mut items = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut marker: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let (mut page, next) = fetch(marker.take()).await?;
        items.append(&mut page);
        match next {
            None => return Ok(items),
            Some(next) => {
                if !seen.insert(next.clone()) {
                    return Err(Error::Remote(format!(
                        "IAM {what}: the server repeated a page marker; the list is incomplete"
                    )));
                }
                marker = Some(next);
            }
        }
    }
    Err(Error::Remote(format!(
        "IAM {what}: more than {MAX_PAGES} pages; the list is incomplete"
    )))
}

fn ctx(operation: &str) -> impl FnOnce(Error) -> Error + '_ {
    move |error| error.with_context(format!("IAM {operation}"))
}

fn utc(time: &aws_smithy_types::DateTime) -> Option<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(time.secs(), time.subsec_nanos())
}

// ---- names and documents ----

fn name_ok(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || "_+=,.@-".contains(c))
}

/// Checks a user name: 1 to 64 letters, digits and `_+=,.@-`.
pub fn validate_user_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 64 || !name_ok(name) {
        return Err(Error::Usage(
            "a user name is 1 to 64 letters, digits and the characters _+=,.@-".to_owned(),
        ));
    }
    Ok(())
}

/// Checks a policy name: 1 to 128 letters, digits and `_+=,.@-`.
pub fn validate_policy_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 || !name_ok(name) {
        return Err(Error::Usage(
            "a policy name is 1 to 128 letters, digits and the characters _+=,.@-".to_owned(),
        ));
    }
    Ok(())
}

/// Checks a user path: `/` or `/segment/.../`, at most 512 characters.
pub fn validate_path(path: &str) -> Result<()> {
    let ok = path.len() <= 512
        && path.starts_with('/')
        && path.ends_with('/')
        && path.chars().all(|c| ('\u{21}'..='\u{7e}').contains(&c))
        && !path.contains("//");
    if !ok {
        return Err(Error::Usage(
            "a path starts and ends with / and holds printable characters, for example /team/"
                .to_owned(),
        ));
    }
    Ok(())
}

fn validate_policy_arn(arn: &str) -> Result<()> {
    if !arn.starts_with("arn:") || !arn.contains(":policy/") || arn.len() > 2048 {
        return Err(Error::Usage(
            "a managed policy ARN looks like arn:aws:iam::aws:policy/ReadOnlyAccess".to_owned(),
        ));
    }
    Ok(())
}

fn validate_access_key_id(id: &str) -> Result<()> {
    if id.len() < 16 || id.len() > 128 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(Error::Usage("not an access key ID".to_owned()));
    }
    Ok(())
}

fn require_typed(what: &str, expected: &str, typed: &str) -> Result<()> {
    if typed == expected {
        Ok(())
    } else {
        Err(Error::Usage(format!(
            "{what} needs a typed confirmation: type {expected} to confirm"
        )))
    }
}

/// Checks an inline policy document the way a user policy needs it, and
/// reports every problem with its path: valid JSON, an object with
/// `Statement`, each statement with `Effect` (`Allow` or `Deny`), one of
/// `Action` and `NotAction`, one of `Resource` and `NotResource`, and no
/// more than [`MAX_INLINE_POLICY_CHARS`] characters without whitespace. A
/// user policy has no `Principal`. IAM has the last word.
pub fn check_user_policy(text: &str) -> Vec<PolicyIssue> {
    let issue = |path: &str, message: String| PolicyIssue {
        path: path.to_owned(),
        message,
    };
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => return vec![issue("", format!("not valid JSON: {error}"))],
    };
    let Some(document) = value.as_object() else {
        return vec![issue("", "the policy must be a JSON object".to_owned())];
    };
    let mut issues = Vec::new();
    let size = compact_json(&value).chars().count();
    if size > MAX_INLINE_POLICY_CHARS {
        issues.push(issue(
            "",
            format!(
                "the policy is {size} characters without whitespace; IAM accepts at most \
                 {MAX_INLINE_POLICY_CHARS} for the inline policies of a user"
            ),
        ));
    }
    match document.get("Version").and_then(|v| v.as_str()) {
        Some("2012-10-17" | "2008-10-17") => {}
        Some(other) => issues.push(issue(
            "Version",
            format!("{other:?} is not a policy language version; use \"2012-10-17\""),
        )),
        None => issues.push(issue(
            "Version",
            "missing; use \"2012-10-17\" so that policy variables work".to_owned(),
        )),
    }
    let statements: Vec<&serde_json::Value> = match document.get("Statement") {
        Some(serde_json::Value::Array(list)) if !list.is_empty() => list.iter().collect(),
        Some(single @ serde_json::Value::Object(_)) => vec![single],
        Some(_) => {
            issues.push(issue(
                "Statement",
                "must be a statement or a non-empty list of statements".to_owned(),
            ));
            return issues;
        }
        None => {
            issues.push(issue("Statement", "missing".to_owned()));
            return issues;
        }
    };
    for (index, statement) in statements.iter().enumerate() {
        let path = format!("Statement[{index}]");
        let Some(statement) = statement.as_object() else {
            issues.push(issue(&path, "must be an object".to_owned()));
            continue;
        };
        match statement.get("Effect").and_then(|v| v.as_str()) {
            Some("Allow" | "Deny") => {}
            _ => issues.push(issue(
                &format!("{path}.Effect"),
                "must be \"Allow\" or \"Deny\"".to_owned(),
            )),
        }
        for (a, b) in [("Action", "NotAction"), ("Resource", "NotResource")] {
            match (statement.contains_key(a), statement.contains_key(b)) {
                (true, true) => issues.push(issue(&path, format!("use {a} or {b}, not both"))),
                (false, false) => issues.push(issue(&path, format!("{a} or {b} is missing"))),
                _ => {}
            }
        }
        for key in ["Principal", "NotPrincipal"] {
            if statement.contains_key(key) {
                issues.push(issue(
                    &format!("{path}.{key}"),
                    "a policy attached to a user has no principal".to_owned(),
                ));
            }
        }
    }
    issues
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Validates a user policy ([`check_user_policy`]) and returns it without
/// whitespace, the form [`Iam::put_inline_policy`] sends. Fails with
/// `usage`, listing every problem.
pub fn compact_user_policy(text: &str) -> Result<String> {
    let issues = check_user_policy(text);
    if !issues.is_empty() {
        let list: Vec<String> = issues.iter().map(ToString::to_string).collect();
        return Err(Error::Usage(format!("invalid policy: {}", list.join("; "))));
    }
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| Error::Internal(format!("reading a checked policy: {error}")))?;
    Ok(compact_json(&value))
}

/// Pretty-prints a policy document, keeping its key order (see
/// [`crate::bucketcfg::format_policy`]).
pub fn format_user_policy(text: &str) -> Result<String> {
    crate::bucketcfg::format_policy(text)
}

// ---- model ----

/// An IAM user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IamUser {
    /// The user name.
    pub name: String,
    /// The ARN.
    pub arn: String,
    /// The stable user ID.
    pub user_id: String,
    /// The path, `/` by default.
    pub path: String,
    /// When the user was created.
    pub created: Option<DateTime<Utc>>,
    /// When the console password was last used.
    pub password_last_used: Option<DateTime<Utc>>,
}

impl From<&aws_sdk_iam::types::User> for IamUser {
    fn from(user: &aws_sdk_iam::types::User) -> Self {
        IamUser {
            name: user.user_name().to_owned(),
            arn: user.arn().to_owned(),
            user_id: user.user_id().to_owned(),
            path: user.path().to_owned(),
            created: utc(user.create_date()),
            password_last_used: user.password_last_used().and_then(utc),
        }
    }
}

/// A managed policy, as attached to a user or offered for attaching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedPolicy {
    /// The policy name.
    pub name: String,
    /// The ARN.
    pub arn: String,
    /// How many identities it is attached to (`None` for an attached-policy
    /// listing, which does not say).
    pub attachment_count: Option<i32>,
    /// The description, when the policy has one.
    pub description: Option<String>,
}

/// Which managed policies [`Iam::list_managed_policies`] lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyScope {
    /// The policies of this account.
    Local,
    /// The policies AWS manages.
    Aws,
    /// Both.
    All,
}

/// An inline policy of a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlinePolicy {
    /// The policy name.
    pub name: String,
    /// The document, pretty-printed.
    pub document: String,
}

/// Whether an access key can be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    /// The key signs requests.
    Active,
    /// The key is kept but refused.
    Inactive,
}

/// An access key of a user, without its secret (IAM never shows it again).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessKeyInfo {
    /// The access key ID.
    pub access_key_id: String,
    /// Whether it can be used.
    pub status: KeyStatus,
    /// When it was created.
    pub created: Option<DateTime<Utc>>,
}

/// A freshly created access key. This is the only time the secret exists;
/// see the [module docs](self#secrets).
#[derive(ZeroizeOnDrop)]
pub struct NewAccessKey {
    /// The user the key belongs to.
    #[zeroize(skip)]
    pub user_name: String,
    /// The access key ID.
    #[zeroize(skip)]
    pub access_key_id: String,
    /// The secret access key.
    pub secret_access_key: SecretString,
    /// When it was created.
    #[zeroize(skip)]
    pub created: Option<DateTime<Utc>>,
}

impl fmt::Debug for NewAccessKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewAccessKey")
            .field("user_name", &self.user_name)
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("created", &self.created)
            .finish()
    }
}

impl Serialize for NewAccessKey {
    /// Serializes the secret: the one place it leaves the core, for the
    /// one-time display. The caller must not keep or log the result.
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("NewAccessKey", 4)?;
        state.serialize_field("user_name", &self.user_name)?;
        state.serialize_field("access_key_id", &self.access_key_id)?;
        state.serialize_field("secret_access_key", self.secret_access_key.expose())?;
        state.serialize_field("created", &self.created)?;
        state.end()
    }
}

/// The console login profile of a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginProfile {
    /// When the password was set.
    pub created: Option<DateTime<Utc>>,
    /// Whether the user must choose a new password at the next sign-in.
    pub password_reset_required: bool,
}

/// A group a user belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRef {
    /// The group name.
    pub name: String,
    /// The ARN.
    pub arn: String,
}

/// Everything about one user, each part read on its own so that a denied
/// part never hides the others.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserDetails {
    /// The user.
    pub user: IamUser,
    /// Attached managed policies.
    pub managed_policies: Section<Vec<ManagedPolicy>>,
    /// Names of the inline policies.
    pub inline_policies: Section<Vec<String>>,
    /// Access keys (no secrets).
    pub access_keys: Section<Vec<AccessKeyInfo>>,
    /// The login profile; `null` when the user has no console password.
    pub login_profile: Section<Option<LoginProfile>>,
    /// Group memberships.
    pub groups: Section<Vec<GroupRef>>,
}

/// One step of [`Iam::delete_user`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteStep {
    /// What the step did: `access_key`, `inline_policy`, `managed_policy`,
    /// `login_profile`, `group_membership`, `user`.
    pub kind: String,
    /// The item it acted on (key ID, policy name or ARN, group name, user
    /// name); empty for the login profile.
    pub target: String,
    /// Whether the item is gone (an item that was already gone counts).
    pub ok: bool,
    /// Why it failed.
    pub error: Option<BbError>,
}

/// The result of [`Iam::delete_user`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteUserOutcome {
    /// The user that was deleted (or not).
    pub user_name: String,
    /// Every step, in the order they ran; the last one deletes the user.
    pub steps: Vec<DeleteStep>,
    /// Whether the user is deleted.
    pub deleted: bool,
}

// ---- client ----

/// The IAM operations of the manager (see the [module docs](self)).
#[derive(Clone)]
pub struct Iam {
    client: aws_sdk_iam::Client,
    read_timeout: Duration,
}

impl fmt::Debug for Iam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Iam")
            .field("read_timeout", &self.read_timeout)
            .finish_non_exhaustive()
    }
}

impl Iam {
    /// A client for `account` with `credentials`. `http_client` is the
    /// proxy-aware client of [`crate::proxy`] (`None` uses the SDK's).
    /// Fails with `usage` for an account with a custom endpoint.
    pub fn new(
        account: &Account,
        credentials: &Credentials,
        options: &ClientOptions,
        http_client: Option<SharedHttpClient>,
    ) -> Result<Self> {
        let mut builder = iam_config(account, credentials, options)?;
        if let Some(http_client) = http_client {
            builder = builder.http_client(http_client);
        }
        Ok(Iam::from_config(builder.build(), options.read_timeout))
    }

    /// A client from a finished configuration (tests inject a mock HTTP
    /// client this way).
    pub fn from_config(config: aws_sdk_iam::Config, read_timeout: Duration) -> Self {
        Iam {
            client: aws_sdk_iam::Client::from_conf(config),
            read_timeout,
        }
    }

    fn timeout(&self) -> aws_sdk_iam::config::Builder {
        aws_sdk_iam::config::Builder::default().timeout_config(
            TimeoutConfig::builder()
                .read_timeout(self.read_timeout)
                .build(),
        )
    }

    // ---- users ----

    /// Lists the users (optionally only those under `path_prefix`).
    pub async fn list_users(&self, path_prefix: Option<&str>) -> Result<Vec<IamUser>> {
        if let Some(prefix) = path_prefix {
            validate_path(prefix)?;
        }
        paged("ListUsers", |marker| async move {
            let out = self
                .client
                .list_users()
                .set_path_prefix(path_prefix.map(str::to_owned))
                .set_marker(marker)
                .max_items(PAGE_SIZE)
                .customize()
                .config_override(self.timeout())
                .send()
                .await
                .map_err(Error::from)
                .map_err(ctx("ListUsers"))?;
            let next = out
                .is_truncated()
                .then(|| out.marker().map(str::to_owned))
                .flatten();
            let users = out.users().iter().map(IamUser::from).collect();
            Ok((users, next))
        })
        .await
    }

    /// Reads one user.
    pub async fn get_user(&self, name: &str) -> Result<IamUser> {
        validate_user_name(name)?;
        let out = self
            .client
            .get_user()
            .user_name(name)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map_err(Error::from)
            .map_err(ctx("GetUser"))?;
        out.user()
            .map(IamUser::from)
            .ok_or_else(|| Error::Remote("IAM GetUser: the answer has no user".to_owned()))
    }

    /// Creates a user (`path` defaults to `/`).
    pub async fn create_user(&self, name: &str, path: Option<&str>) -> Result<IamUser> {
        validate_user_name(name)?;
        if let Some(path) = path {
            validate_path(path)?;
        }
        let out = self
            .client
            .create_user()
            .user_name(name)
            .set_path(path.map(str::to_owned))
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map_err(Error::from)
            .map_err(ctx("CreateUser"))?;
        out.user()
            .map(IamUser::from)
            .ok_or_else(|| Error::Remote("IAM CreateUser: the answer has no user".to_owned()))
    }

    /// Renames the user and/or moves it to another path. Returns the user
    /// as it is afterwards.
    pub async fn update_user(
        &self,
        name: &str,
        new_name: Option<&str>,
        new_path: Option<&str>,
    ) -> Result<IamUser> {
        validate_user_name(name)?;
        if let Some(new_name) = new_name {
            validate_user_name(new_name)?;
        }
        if let Some(new_path) = new_path {
            validate_path(new_path)?;
        }
        if new_name.is_none() && new_path.is_none() {
            return Err(Error::Usage("nothing to change".to_owned()));
        }
        self.client
            .update_user()
            .user_name(name)
            .set_new_user_name(new_name.map(str::to_owned))
            .set_new_path(new_path.map(str::to_owned))
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map_err(Error::from)
            .map_err(ctx("UpdateUser"))?;
        self.get_user(new_name.unwrap_or(name)).await
    }

    /// Reads a user and everything attached to it.
    pub async fn user_details(&self, name: &str) -> Result<UserDetails> {
        let user = self.get_user(name).await?;
        let (managed, inline, keys, profile, groups) = tokio::join!(
            self.list_attached_policies(name),
            self.list_inline_policies(name),
            self.list_access_keys(name),
            self.get_login_profile(name),
            self.list_groups(name),
        );
        Ok(UserDetails {
            user,
            managed_policies: Section::from_result(managed, "managed policies"),
            inline_policies: Section::from_result(inline, "inline policies"),
            access_keys: Section::from_result(keys, "access keys"),
            login_profile: Section::from_result(profile, "login profiles"),
            groups: Section::from_result(groups, "groups"),
        })
    }

    /// Deletes a user after removing everything it owns (see the
    /// [module docs](self#deleting-a-user)). `confirm` must equal `name`.
    pub async fn delete_user(&self, name: &str, confirm: &str) -> Result<DeleteUserOutcome> {
        validate_user_name(name)?;
        require_typed("deleting a user", name, confirm)?;
        let mut steps: Vec<DeleteStep> = Vec::new();
        // Whether the user exists is the first thing to know.
        self.get_user(name).await?;

        match self.list_access_keys(name).await {
            Ok(keys) => {
                for key in keys {
                    let result = self.remove_access_key(name, &key.access_key_id).await;
                    steps.push(step("access_key", &key.access_key_id, result));
                }
            }
            Err(error) => steps.push(step("access_key", "", Err(error))),
        }
        match self.list_inline_policies(name).await {
            Ok(names) => {
                for policy in names {
                    let result = self.remove_inline_policy(name, &policy).await;
                    steps.push(step("inline_policy", &policy, result));
                }
            }
            Err(error) => steps.push(step("inline_policy", "", Err(error))),
        }
        match self.list_attached_policies(name).await {
            Ok(policies) => {
                for policy in policies {
                    let result = self.detach_user_policy(name, &policy.arn).await;
                    steps.push(step("managed_policy", &policy.arn, result));
                }
            }
            Err(error) => steps.push(step("managed_policy", "", Err(error))),
        }
        let profile = match self.remove_login_profile(name).await {
            Err(error) if error.kind() == crate::error::ErrorKind::NotFound => Ok(()),
            other => other,
        };
        steps.push(step("login_profile", "", profile));
        match self.list_groups(name).await {
            Ok(groups) => {
                for group in groups {
                    let result = self.remove_from_group(name, &group.name).await;
                    steps.push(step("group_membership", &group.name, result));
                }
            }
            Err(error) => steps.push(step("group_membership", "", Err(error))),
        }

        let deleted = if steps.iter().all(|s| s.ok) {
            let result = self
                .client
                .delete_user()
                .user_name(name)
                .customize()
                .config_override(self.timeout())
                .send()
                .await
                .map(|_| ())
                .map_err(Error::from)
                .map_err(ctx("DeleteUser"));
            let ok = result.is_ok();
            steps.push(step("user", name, result));
            ok
        } else {
            false
        };
        Ok(DeleteUserOutcome {
            user_name: name.to_owned(),
            steps,
            deleted,
        })
    }

    // ---- managed policies ----

    /// The managed policies attached to a user.
    pub async fn list_attached_policies(&self, user: &str) -> Result<Vec<ManagedPolicy>> {
        validate_user_name(user)?;
        paged("ListAttachedUserPolicies", |marker| async move {
            let out = self
                .client
                .list_attached_user_policies()
                .user_name(user)
                .set_marker(marker)
                .max_items(PAGE_SIZE)
                .customize()
                .config_override(self.timeout())
                .send()
                .await
                .map_err(Error::from)
                .map_err(ctx("ListAttachedUserPolicies"))?;
            let next = out
                .is_truncated()
                .then(|| out.marker().map(str::to_owned))
                .flatten();
            let items = out
                .attached_policies()
                .iter()
                .map(|policy| ManagedPolicy {
                    name: policy.policy_name().unwrap_or_default().to_owned(),
                    arn: policy.policy_arn().unwrap_or_default().to_owned(),
                    attachment_count: None,
                    description: None,
                })
                .collect();
            Ok((items, next))
        })
        .await
    }

    /// Lists managed policies that can be attached (`scope` picks the
    /// customer-managed ones, the AWS-managed ones or both).
    pub async fn list_managed_policies(&self, scope: PolicyScope) -> Result<Vec<ManagedPolicy>> {
        let scope = match scope {
            PolicyScope::Local => PolicyScopeType::Local,
            PolicyScope::Aws => PolicyScopeType::Aws,
            PolicyScope::All => PolicyScopeType::All,
        };
        paged("ListPolicies", |marker| {
            let scope = scope.clone();
            async move {
                let out = self
                    .client
                    .list_policies()
                    .scope(scope)
                    .set_marker(marker)
                    .max_items(PAGE_SIZE)
                    .customize()
                    .config_override(self.timeout())
                    .send()
                    .await
                    .map_err(Error::from)
                    .map_err(ctx("ListPolicies"))?;
                let next = out
                    .is_truncated()
                    .then(|| out.marker().map(str::to_owned))
                    .flatten();
                let items = out
                    .policies()
                    .iter()
                    .map(|policy| ManagedPolicy {
                        name: policy.policy_name().unwrap_or_default().to_owned(),
                        arn: policy.arn().unwrap_or_default().to_owned(),
                        attachment_count: policy.attachment_count(),
                        description: policy.description().map(str::to_owned),
                    })
                    .collect();
                Ok((items, next))
            }
        })
        .await
    }

    /// Attaches a managed policy to a user.
    pub async fn attach_user_policy(&self, user: &str, policy_arn: &str) -> Result<()> {
        validate_user_name(user)?;
        validate_policy_arn(policy_arn)?;
        self.client
            .attach_user_policy()
            .user_name(user)
            .policy_arn(policy_arn)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("AttachUserPolicy"))
    }

    /// Detaches a managed policy from a user.
    pub async fn detach_user_policy(&self, user: &str, policy_arn: &str) -> Result<()> {
        validate_user_name(user)?;
        validate_policy_arn(policy_arn)?;
        self.client
            .detach_user_policy()
            .user_name(user)
            .policy_arn(policy_arn)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("DetachUserPolicy"))
    }

    // ---- inline policies ----

    /// The names of the inline policies of a user.
    pub async fn list_inline_policies(&self, user: &str) -> Result<Vec<String>> {
        validate_user_name(user)?;
        paged("ListUserPolicies", |marker| async move {
            let out = self
                .client
                .list_user_policies()
                .user_name(user)
                .set_marker(marker)
                .max_items(PAGE_SIZE)
                .customize()
                .config_override(self.timeout())
                .send()
                .await
                .map_err(Error::from)
                .map_err(ctx("ListUserPolicies"))?;
            let next = out
                .is_truncated()
                .then(|| out.marker().map(str::to_owned))
                .flatten();
            Ok((out.policy_names().to_vec(), next))
        })
        .await
    }

    /// Reads an inline policy; the document is decoded (IAM sends it
    /// percent-encoded) and pretty-printed.
    pub async fn get_inline_policy(&self, user: &str, policy: &str) -> Result<InlinePolicy> {
        validate_user_name(user)?;
        validate_policy_name(policy)?;
        let out = self
            .client
            .get_user_policy()
            .user_name(user)
            .policy_name(policy)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map_err(Error::from)
            .map_err(ctx("GetUserPolicy"))?;
        let decoded = percent_encoding::percent_decode_str(out.policy_document())
            .decode_utf8()
            .map_err(|error| {
                Error::Remote(format!(
                    "IAM GetUserPolicy: the document is not UTF-8: {error}"
                ))
            })?;
        let document = format_user_policy(&decoded).unwrap_or_else(|_| decoded.into_owned());
        Ok(InlinePolicy {
            name: out.policy_name().to_owned(),
            document,
        })
    }

    /// Creates or replaces an inline policy. The document is validated
    /// first ([`check_user_policy`]) and nothing is sent when it has
    /// problems.
    pub async fn put_inline_policy(&self, user: &str, policy: &str, document: &str) -> Result<()> {
        validate_user_name(user)?;
        validate_policy_name(policy)?;
        let compact = compact_user_policy(document)?;
        self.client
            .put_user_policy()
            .user_name(user)
            .policy_name(policy)
            .policy_document(compact)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("PutUserPolicy"))
    }

    /// Deletes an inline policy. `confirm` must equal `policy`.
    pub async fn delete_inline_policy(
        &self,
        user: &str,
        policy: &str,
        confirm: &str,
    ) -> Result<()> {
        validate_user_name(user)?;
        validate_policy_name(policy)?;
        require_typed("deleting a policy", policy, confirm)?;
        self.remove_inline_policy(user, policy).await
    }

    async fn remove_inline_policy(&self, user: &str, policy: &str) -> Result<()> {
        self.client
            .delete_user_policy()
            .user_name(user)
            .policy_name(policy)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("DeleteUserPolicy"))
    }

    // ---- access keys ----

    /// The access keys of a user (IAM allows two).
    pub async fn list_access_keys(&self, user: &str) -> Result<Vec<AccessKeyInfo>> {
        validate_user_name(user)?;
        paged("ListAccessKeys", |marker| async move {
            let out = self
                .client
                .list_access_keys()
                .user_name(user)
                .set_marker(marker)
                .max_items(PAGE_SIZE)
                .customize()
                .config_override(self.timeout())
                .send()
                .await
                .map_err(Error::from)
                .map_err(ctx("ListAccessKeys"))?;
            let next = out
                .is_truncated()
                .then(|| out.marker().map(str::to_owned))
                .flatten();
            let items = out
                .access_key_metadata()
                .iter()
                .map(|key| AccessKeyInfo {
                    access_key_id: key.access_key_id().unwrap_or_default().to_owned(),
                    status: match key.status() {
                        Some(StatusType::Inactive) => KeyStatus::Inactive,
                        _ => KeyStatus::Active,
                    },
                    created: key.create_date().and_then(utc),
                })
                .collect();
            Ok((items, next))
        })
        .await
    }

    /// Creates an access key. The secret is in the result and nowhere else:
    /// show it once and drop it (see the [module docs](self#secrets)).
    pub async fn create_access_key(&self, user: &str) -> Result<NewAccessKey> {
        validate_user_name(user)?;
        let out = self
            .client
            .create_access_key()
            .user_name(user)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map_err(Error::from)
            .map_err(ctx("CreateAccessKey"))?;
        let key = out.access_key().ok_or_else(|| {
            Error::Remote("IAM CreateAccessKey: the answer has no access key".to_owned())
        })?;
        Ok(NewAccessKey {
            user_name: key.user_name().to_owned(),
            access_key_id: key.access_key_id().to_owned(),
            secret_access_key: SecretString::new(key.secret_access_key().to_owned()),
            created: key.create_date().and_then(utc),
        })
    }

    /// Activates or deactivates an access key.
    pub async fn set_access_key_active(
        &self,
        user: &str,
        access_key_id: &str,
        active: bool,
    ) -> Result<()> {
        validate_user_name(user)?;
        validate_access_key_id(access_key_id)?;
        self.client
            .update_access_key()
            .user_name(user)
            .access_key_id(access_key_id)
            .status(if active {
                StatusType::Active
            } else {
                StatusType::Inactive
            })
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("UpdateAccessKey"))
    }

    /// Deletes an access key. `confirm` must equal `access_key_id`.
    pub async fn delete_access_key(
        &self,
        user: &str,
        access_key_id: &str,
        confirm: &str,
    ) -> Result<()> {
        validate_user_name(user)?;
        validate_access_key_id(access_key_id)?;
        require_typed("deleting an access key", access_key_id, confirm)?;
        self.remove_access_key(user, access_key_id).await
    }

    async fn remove_access_key(&self, user: &str, access_key_id: &str) -> Result<()> {
        self.client
            .delete_access_key()
            .user_name(user)
            .access_key_id(access_key_id)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("DeleteAccessKey"))
    }

    // ---- login profile ----

    /// The console login profile of a user; `None` when there is none.
    pub async fn get_login_profile(&self, user: &str) -> Result<Option<LoginProfile>> {
        validate_user_name(user)?;
        match self
            .client
            .get_login_profile()
            .user_name(user)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
        {
            Ok(out) => Ok(out.login_profile().map(|profile| LoginProfile {
                created: utc(profile.create_date()),
                password_reset_required: profile.password_reset_required(),
            })),
            Err(error) => {
                let error = Error::from(error);
                if error.kind() == crate::error::ErrorKind::NotFound
                    && error.service().and_then(|s| s.code()) == Some("NoSuchEntity")
                {
                    Ok(None)
                } else {
                    Err(ctx("GetLoginProfile")(error))
                }
            }
        }
    }

    /// Gives the user a console password.
    pub async fn create_login_profile(
        &self,
        user: &str,
        password: &SecretString,
        reset_required: bool,
    ) -> Result<()> {
        validate_user_name(user)?;
        validate_password(password)?;
        self.client
            .create_login_profile()
            .user_name(user)
            .password(password.expose())
            .password_reset_required(reset_required)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("CreateLoginProfile"))
    }

    /// Changes the console password and/or the reset requirement.
    pub async fn update_login_profile(
        &self,
        user: &str,
        password: Option<&SecretString>,
        reset_required: Option<bool>,
    ) -> Result<()> {
        validate_user_name(user)?;
        if let Some(password) = password {
            validate_password(password)?;
        }
        if password.is_none() && reset_required.is_none() {
            return Err(Error::Usage("nothing to change".to_owned()));
        }
        self.client
            .update_login_profile()
            .user_name(user)
            .set_password(password.map(|p| p.expose().to_owned()))
            .set_password_reset_required(reset_required)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("UpdateLoginProfile"))
    }

    /// Removes the console password. `confirm` must equal the user name.
    pub async fn delete_login_profile(&self, user: &str, confirm: &str) -> Result<()> {
        validate_user_name(user)?;
        require_typed("deleting a login profile", user, confirm)?;
        self.remove_login_profile(user).await
    }

    async fn remove_login_profile(&self, user: &str) -> Result<()> {
        self.client
            .delete_login_profile()
            .user_name(user)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("DeleteLoginProfile"))
    }

    // ---- groups ----

    /// The groups a user belongs to.
    pub async fn list_groups(&self, user: &str) -> Result<Vec<GroupRef>> {
        validate_user_name(user)?;
        paged("ListGroupsForUser", |marker| async move {
            let out = self
                .client
                .list_groups_for_user()
                .user_name(user)
                .set_marker(marker)
                .max_items(PAGE_SIZE)
                .customize()
                .config_override(self.timeout())
                .send()
                .await
                .map_err(Error::from)
                .map_err(ctx("ListGroupsForUser"))?;
            let next = out
                .is_truncated()
                .then(|| out.marker().map(str::to_owned))
                .flatten();
            let items = out
                .groups()
                .iter()
                .map(|group| GroupRef {
                    name: group.group_name().to_owned(),
                    arn: group.arn().to_owned(),
                })
                .collect();
            Ok((items, next))
        })
        .await
    }

    async fn remove_from_group(&self, user: &str, group: &str) -> Result<()> {
        self.client
            .remove_user_from_group()
            .user_name(user)
            .group_name(group)
            .customize()
            .config_override(self.timeout())
            .send()
            .await
            .map(|_| ())
            .map_err(Error::from)
            .map_err(ctx("RemoveUserFromGroup"))
    }
}

fn validate_password(password: &SecretString) -> Result<()> {
    let length = password.expose().chars().count();
    if length == 0 || length > 128 {
        return Err(Error::Usage(
            "a console password is 1 to 128 characters".to_owned(),
        ));
    }
    Ok(())
}

fn step(kind: &str, target: &str, result: Result<()>) -> DeleteStep {
    DeleteStep {
        kind: kind.to_owned(),
        target: target.to_owned(),
        ok: result.is_ok(),
        error: result.err().map(|error| error.to_payload()),
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_iam::operation::{
        attach_user_policy::AttachUserPolicyOutput, create_access_key::CreateAccessKeyOutput,
        create_login_profile::CreateLoginProfileOutput, create_user::CreateUserOutput,
        delete_access_key::DeleteAccessKeyOutput, delete_login_profile::DeleteLoginProfileOutput,
        delete_user::DeleteUserOutput, delete_user_policy::DeleteUserPolicyOutput,
        detach_user_policy::DetachUserPolicyOutput, get_login_profile::GetLoginProfileOutput,
        get_user::GetUserOutput, get_user_policy::GetUserPolicyOutput,
        list_access_keys::ListAccessKeysOutput,
        list_attached_user_policies::ListAttachedUserPoliciesOutput,
        list_groups_for_user::ListGroupsForUserOutput, list_policies::ListPoliciesOutput,
        list_user_policies::ListUserPoliciesOutput, list_users::ListUsersOutput,
        put_user_policy::PutUserPolicyOutput, remove_user_from_group::RemoveUserFromGroupOutput,
        update_access_key::UpdateAccessKeyOutput, update_login_profile::UpdateLoginProfileOutput,
        update_user::UpdateUserOutput,
    };
    use aws_sdk_iam::types::{
        AccessKey, AccessKeyMetadata, AttachedPolicy, Group, LoginProfile as SdkLoginProfile,
        Policy, User,
    };
    use aws_smithy_mocks::{
        MockResponseInterceptor, Rule, RuleMode, create_mock_http_client, mock,
    };
    use aws_smithy_runtime_api::client::orchestrator::HttpResponse;
    use aws_smithy_runtime_api::http::StatusCode;
    use aws_smithy_types::body::SdkBody;

    use super::*;
    use crate::error::ErrorKind;

    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYNEWSECRETKEY";
    const KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";
    const ARN: &str = "arn:aws:iam::aws:policy/ReadOnlyAccess";

    fn account(endpoint: Option<&str>) -> Account {
        Account {
            source: crate::model::CredentialSource::StaticKeys,
            id: "account-a".to_owned(),
            label: "label".to_owned(),
            access_key_id: "AKIAADMINEXAMPLE0000".to_owned(),
            secret_ref: "account-a/secret/1".to_owned(),
            session_token_ref: None,
            region: "eu-west-1".to_owned(),
            endpoint: endpoint.map(str::to_owned),
            path_style: false,
            request_payer: false,
            mfa_serial: None,
        }
    }

    fn credentials() -> Credentials {
        Credentials::new("AKIAADMINEXAMPLE0000", "admin-secret-value", None)
    }

    fn mock_iam(rules: &[&Rule]) -> Iam {
        mock_iam_with(RuleMode::Sequential, rules)
    }

    fn mock_iam_with(mode: RuleMode, rules: &[&Rule]) -> Iam {
        let mut interceptor = MockResponseInterceptor::new().rule_mode(mode);
        for rule in rules {
            interceptor = interceptor.with_rule(rule);
        }
        let config = iam_config(&account(None), &credentials(), &ClientOptions::default())
            .unwrap()
            .interceptor(interceptor)
            .http_client(create_mock_http_client())
            .build();
        Iam::from_config(config, Duration::from_secs(5))
    }

    fn user(name: &str) -> User {
        User::builder()
            .path("/")
            .user_name(name)
            .user_id(format!("AIDA{name}"))
            .arn(format!("arn:aws:iam::123456789012:user/{name}"))
            .create_date(aws_smithy_types::DateTime::from_secs(1_700_000_000))
            .build()
            .unwrap()
    }

    fn error_rule_response(status: u16, code: &str) -> impl Fn() -> HttpResponse + use<> {
        let body = format!(
            "<ErrorResponse xmlns=\"https://iam.amazonaws.com/doc/2010-05-08/\"><Error>\
             <Type>Sender</Type><Code>{code}</Code><Message>example</Message></Error>\
             <RequestId>r-1</RequestId></ErrorResponse>"
        );
        move || {
            HttpResponse::new(
                StatusCode::try_from(status).unwrap(),
                SdkBody::from(body.clone()),
            )
        }
    }

    fn rules_for_a_full_user() -> Vec<Rule> {
        vec![
            mock!(aws_sdk_iam::Client::get_user)
                .match_requests(|i| i.user_name() == Some("alice"))
                .then_output(|| GetUserOutput::builder().user(user("alice")).build()),
            mock!(aws_sdk_iam::Client::list_access_keys).then_output(|| {
                ListAccessKeysOutput::builder()
                    .access_key_metadata(
                        AccessKeyMetadata::builder()
                            .access_key_id(KEY_ID)
                            .status(StatusType::Active)
                            .build(),
                    )
                    .is_truncated(false)
                    .build()
                    .unwrap()
            }),
            mock!(aws_sdk_iam::Client::delete_access_key)
                .match_requests(|i| i.access_key_id() == Some(KEY_ID))
                .then_output(|| DeleteAccessKeyOutput::builder().build()),
            mock!(aws_sdk_iam::Client::list_user_policies).then_output(|| {
                ListUserPoliciesOutput::builder()
                    .policy_names("inline-one")
                    .is_truncated(false)
                    .build()
                    .unwrap()
            }),
            mock!(aws_sdk_iam::Client::delete_user_policy)
                .match_requests(|i| i.policy_name() == Some("inline-one"))
                .then_output(|| DeleteUserPolicyOutput::builder().build()),
            mock!(aws_sdk_iam::Client::list_attached_user_policies).then_output(|| {
                ListAttachedUserPoliciesOutput::builder()
                    .attached_policies(
                        AttachedPolicy::builder()
                            .policy_name("ReadOnlyAccess")
                            .policy_arn(ARN)
                            .build(),
                    )
                    .is_truncated(false)
                    .build()
            }),
            mock!(aws_sdk_iam::Client::detach_user_policy)
                .match_requests(|i| i.policy_arn() == Some(ARN))
                .then_output(|| DetachUserPolicyOutput::builder().build()),
            mock!(aws_sdk_iam::Client::delete_login_profile)
                .then_http_response(error_rule_response(404, "NoSuchEntity")),
            mock!(aws_sdk_iam::Client::list_groups_for_user).then_output(|| {
                ListGroupsForUserOutput::builder()
                    .groups(
                        Group::builder()
                            .path("/")
                            .group_name("admins")
                            .group_id("AGPA1")
                            .arn("arn:aws:iam::123456789012:group/admins")
                            .create_date(aws_smithy_types::DateTime::from_secs(1))
                            .build()
                            .unwrap(),
                    )
                    .is_truncated(false)
                    .build()
                    .unwrap()
            }),
            mock!(aws_sdk_iam::Client::remove_user_from_group)
                .match_requests(|i| i.group_name() == Some("admins"))
                .then_output(|| RemoveUserFromGroupOutput::builder().build()),
            mock!(aws_sdk_iam::Client::delete_user)
                .match_requests(|i| i.user_name() == Some("alice"))
                .then_output(|| DeleteUserOutput::builder().build()),
        ]
    }

    #[tokio::test]
    async fn create_user_key_policy_then_delete() {
        let create = mock!(aws_sdk_iam::Client::create_user)
            .match_requests(|i| i.user_name() == Some("alice") && i.path() == Some("/team/"))
            .then_output(|| CreateUserOutput::builder().user(user("alice")).build());
        let key = mock!(aws_sdk_iam::Client::create_access_key)
            .match_requests(|i| i.user_name() == Some("alice"))
            .then_output(|| {
                CreateAccessKeyOutput::builder()
                    .access_key(
                        AccessKey::builder()
                            .user_name("alice")
                            .access_key_id(KEY_ID)
                            .status(StatusType::Active)
                            .secret_access_key(SECRET)
                            .create_date(aws_smithy_types::DateTime::from_secs(1_700_000_001))
                            .build()
                            .unwrap(),
                    )
                    .build()
            });
        let attach = mock!(aws_sdk_iam::Client::attach_user_policy)
            .match_requests(|i| i.user_name() == Some("alice") && i.policy_arn() == Some(ARN))
            .then_output(|| AttachUserPolicyOutput::builder().build());
        let rules = rules_for_a_full_user();
        let mut all: Vec<&Rule> = vec![&create, &key, &attach];
        all.extend(rules.iter());
        let iam = mock_iam(&all);

        let created = iam.create_user("alice", Some("/team/")).await.unwrap();
        assert_eq!(created.name, "alice");
        let new_key = iam.create_access_key("alice").await.unwrap();
        assert_eq!(new_key.access_key_id, KEY_ID);
        assert_eq!(new_key.secret_access_key.expose(), SECRET);
        iam.attach_user_policy("alice", ARN).await.unwrap();

        let outcome = iam.delete_user("alice", "alice").await.unwrap();
        assert!(outcome.deleted);
        let kinds: Vec<&str> = outcome.steps.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "access_key",
                "inline_policy",
                "managed_policy",
                "login_profile",
                "group_membership",
                "user"
            ]
        );
        assert!(outcome.steps.iter().all(|s| s.ok), "{outcome:?}");
        assert_eq!(outcome.steps[0].target, KEY_ID);
        for rule in all {
            assert_eq!(rule.num_calls(), 1);
        }
    }

    #[tokio::test]
    async fn a_failed_cleanup_step_keeps_the_user() {
        let get = mock!(aws_sdk_iam::Client::get_user)
            .then_output(|| GetUserOutput::builder().user(user("alice")).build());
        let keys = mock!(aws_sdk_iam::Client::list_access_keys)
            .then_http_response(error_rule_response(403, "AccessDenied"));
        let inline = mock!(aws_sdk_iam::Client::list_user_policies).then_output(|| {
            ListUserPoliciesOutput::builder()
                .set_policy_names(Some(Vec::new()))
                .is_truncated(false)
                .build()
                .unwrap()
        });
        let attached = mock!(aws_sdk_iam::Client::list_attached_user_policies)
            .then_output(|| ListAttachedUserPoliciesOutput::builder().build());
        let profile = mock!(aws_sdk_iam::Client::delete_login_profile)
            .then_output(|| DeleteLoginProfileOutput::builder().build());
        let groups = mock!(aws_sdk_iam::Client::list_groups_for_user).then_output(|| {
            ListGroupsForUserOutput::builder()
                .set_groups(Some(Vec::new()))
                .is_truncated(false)
                .build()
                .unwrap()
        });
        let delete = mock!(aws_sdk_iam::Client::delete_user)
            .then_output(|| DeleteUserOutput::builder().build());
        let iam = mock_iam(&[&get, &keys, &inline, &attached, &profile, &groups, &delete]);
        let outcome = iam.delete_user("alice", "alice").await.unwrap();
        assert!(!outcome.deleted);
        assert_eq!(delete.num_calls(), 0, "DeleteUser must not run");
        let failed: Vec<_> = outcome.steps.iter().filter(|s| !s.ok).collect();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].kind, "access_key");
        assert_eq!(failed[0].error.as_ref().unwrap().code, ErrorKind::Auth);
        // The other steps still ran.
        assert_eq!(groups.num_calls(), 1);
    }

    #[tokio::test]
    async fn destructive_actions_need_the_typed_name() {
        let any = mock!(aws_sdk_iam::Client::delete_user)
            .then_output(|| DeleteUserOutput::builder().build());
        let iam = mock_iam(&[&any]);
        for error in [
            iam.delete_user("alice", "").await.unwrap_err(),
            iam.delete_user("alice", "Alice").await.unwrap_err(),
            iam.delete_access_key("alice", KEY_ID, "alice")
                .await
                .unwrap_err(),
            iam.delete_inline_policy("alice", "p", "alice")
                .await
                .unwrap_err(),
            iam.delete_login_profile("alice", "bob").await.unwrap_err(),
        ] {
            assert_eq!(error.kind(), ErrorKind::Usage);
            assert!(error.to_string().contains("typed confirmation"), "{error}");
        }
        assert_eq!(any.num_calls(), 0);
    }

    fn users_page(names: &[&str], marker: Option<&'static str>) -> ListUsersOutput {
        let mut out = ListUsersOutput::builder().is_truncated(marker.is_some());
        for name in names {
            out = out.users(user(name));
        }
        if let Some(marker) = marker {
            out = out.marker(marker);
        }
        out.build().unwrap()
    }

    #[tokio::test]
    async fn lists_follow_every_page() {
        let first = mock!(aws_sdk_iam::Client::list_users)
            .match_requests(|i| i.marker().is_none())
            .then_output(|| users_page(&["a", "b"], Some("m1")));
        let second = mock!(aws_sdk_iam::Client::list_users)
            .match_requests(|i| i.marker() == Some("m1"))
            .then_output(|| users_page(&["c"], Some("m2")));
        let third = mock!(aws_sdk_iam::Client::list_users)
            .match_requests(|i| i.marker() == Some("m2"))
            .then_output(|| users_page(&["d"], None));
        let iam = mock_iam(&[&first, &second, &third]);
        let users = iam.list_users(None).await.unwrap();
        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "d"]);
        assert_eq!(users[0].created.unwrap().timestamp(), 1_700_000_000);
    }

    #[tokio::test]
    async fn a_repeated_marker_ends_the_walk() {
        let looping = mock!(aws_sdk_iam::Client::list_users)
            .sequence()
            .output(|| users_page(&["a"], Some("same")))
            .repeatedly()
            .build();
        let iam = mock_iam(&[&looping]);
        let error = iam.list_users(None).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Remote);
        assert!(
            error.to_string().contains("repeated a page marker"),
            "{error}"
        );
        assert_eq!(looping.num_calls(), 2);
    }

    #[tokio::test]
    async fn pages_of_every_other_list_are_followed_too() {
        let a1 = mock!(aws_sdk_iam::Client::list_access_keys)
            .match_requests(|i| i.marker().is_none())
            .then_output(|| {
                ListAccessKeysOutput::builder()
                    .access_key_metadata(
                        AccessKeyMetadata::builder()
                            .access_key_id("AKIAKEYONE0000000001")
                            .status(StatusType::Inactive)
                            .build(),
                    )
                    .is_truncated(true)
                    .marker("k1")
                    .build()
                    .unwrap()
            });
        let a2 = mock!(aws_sdk_iam::Client::list_access_keys)
            .match_requests(|i| i.marker() == Some("k1"))
            .then_output(|| {
                ListAccessKeysOutput::builder()
                    .access_key_metadata(
                        AccessKeyMetadata::builder()
                            .access_key_id("AKIAKEYTWO0000000002")
                            .status(StatusType::Active)
                            .build(),
                    )
                    .is_truncated(false)
                    .build()
                    .unwrap()
            });
        let p1 = mock!(aws_sdk_iam::Client::list_policies)
            .match_requests(|i| i.marker().is_none() && i.scope() == Some(&PolicyScopeType::Local))
            .then_output(|| {
                ListPoliciesOutput::builder()
                    .policies(
                        Policy::builder()
                            .policy_name("one")
                            .arn(ARN)
                            .attachment_count(2)
                            .build(),
                    )
                    .is_truncated(true)
                    .marker("p1")
                    .build()
            });
        let p2 = mock!(aws_sdk_iam::Client::list_policies)
            .match_requests(|i| i.marker() == Some("p1"))
            .then_output(|| {
                ListPoliciesOutput::builder()
                    .policies(Policy::builder().policy_name("two").arn(ARN).build())
                    .is_truncated(false)
                    .build()
            });
        let iam = mock_iam(&[&a1, &a2, &p1, &p2]);
        let keys = iam.list_access_keys("alice").await.unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].status, KeyStatus::Inactive);
        assert_eq!(keys[1].status, KeyStatus::Active);
        let policies = iam.list_managed_policies(PolicyScope::Local).await.unwrap();
        assert_eq!(policies.len(), 2);
        assert_eq!(policies[0].attachment_count, Some(2));
    }

    #[tokio::test]
    async fn edits_a_user() {
        let update = mock!(aws_sdk_iam::Client::update_user)
            .match_requests(|i| {
                i.user_name() == Some("alice")
                    && i.new_user_name() == Some("alicia")
                    && i.new_path().is_none()
            })
            .then_output(|| UpdateUserOutput::builder().build());
        let get = mock!(aws_sdk_iam::Client::get_user)
            .match_requests(|i| i.user_name() == Some("alicia"))
            .then_output(|| GetUserOutput::builder().user(user("alicia")).build());
        let iam = mock_iam(&[&update, &get]);
        assert_eq!(
            iam.update_user("alice", Some("alicia"), None)
                .await
                .unwrap()
                .name,
            "alicia"
        );
        assert_eq!(
            iam.update_user("alice", None, None)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
    }

    #[tokio::test]
    async fn inline_policies_round_trip() {
        let document = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"arn:aws:s3:::b/*"}]}"#;
        let encoded: String =
            percent_encoding::utf8_percent_encode(document, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        let put = mock!(aws_sdk_iam::Client::put_user_policy)
            .match_requests(move |i| {
                i.user_name() == Some("alice")
                    && i.policy_name() == Some("read")
                    && i.policy_document() == Some(document)
            })
            .then_output(|| PutUserPolicyOutput::builder().build());
        let get = mock!(aws_sdk_iam::Client::get_user_policy).then_output(move || {
            GetUserPolicyOutput::builder()
                .user_name("alice")
                .policy_name("read")
                .policy_document(encoded.clone())
                .build()
                .unwrap()
        });
        let delete = mock!(aws_sdk_iam::Client::delete_user_policy)
            .match_requests(|i| i.policy_name() == Some("read"))
            .then_output(|| DeleteUserPolicyOutput::builder().build());
        let iam = mock_iam(&[&put, &get, &delete]);
        // Pretty input is sent compact.
        let pretty = format_user_policy(document).unwrap();
        assert!(pretty.contains('\n'));
        iam.put_inline_policy("alice", "read", &pretty)
            .await
            .unwrap();
        let read = iam.get_inline_policy("alice", "read").await.unwrap();
        assert_eq!(read.name, "read");
        assert!(read.document.contains("\"Action\": \"s3:GetObject\""));
        iam.delete_inline_policy("alice", "read", "read")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_invalid_policy_never_reaches_iam() {
        let put = mock!(aws_sdk_iam::Client::put_user_policy)
            .then_output(|| PutUserPolicyOutput::builder().build());
        let iam = mock_iam(&[&put]);
        for bad in ["", "{", "[]", r#"{"Version":"2012-10-17"}"#] {
            let error = iam.put_inline_policy("alice", "p", bad).await.unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Usage, "{bad}");
        }
        assert_eq!(put.num_calls(), 0);
    }

    #[test]
    fn user_policy_checks_report_paths() {
        let issues = check_user_policy(
            r#"{"Version":"x","Statement":[{"Effect":"Maybe","Action":"a","NotAction":"b","Principal":"*"}]}"#,
        );
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        assert!(paths.contains(&"Version"));
        assert!(paths.contains(&"Statement[0].Effect"));
        assert!(paths.contains(&"Statement[0].Principal"));
        assert!(issues.iter().any(|i| i.message.contains("not both")));
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("Resource or NotResource"))
        );
        let long = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"*","Resource":"{}"}}]}}"#,
            "a".repeat(2100)
        );
        assert!(check_user_policy(&long)[0].message.contains("at most 2048"));
        assert!(
            check_user_policy(
                r#"{"Version":"2012-10-17","Statement":{"Effect":"Deny","NotAction":"s3:*","Resource":"*"}}"#
            )
            .is_empty()
        );
    }

    #[tokio::test]
    async fn access_keys_can_be_deactivated_and_deleted() {
        let update = mock!(aws_sdk_iam::Client::update_access_key)
            .match_requests(|i| {
                i.access_key_id() == Some(KEY_ID) && i.status() == Some(&StatusType::Inactive)
            })
            .then_output(|| UpdateAccessKeyOutput::builder().build());
        let delete = mock!(aws_sdk_iam::Client::delete_access_key)
            .match_requests(|i| i.access_key_id() == Some(KEY_ID) && i.user_name() == Some("alice"))
            .then_output(|| DeleteAccessKeyOutput::builder().build());
        let iam = mock_iam(&[&update, &delete]);
        iam.set_access_key_active("alice", KEY_ID, false)
            .await
            .unwrap();
        iam.delete_access_key("alice", KEY_ID, KEY_ID)
            .await
            .unwrap();
        assert!(
            iam.set_access_key_active("alice", "no id!", true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn login_profiles() {
        let none = mock!(aws_sdk_iam::Client::get_login_profile)
            .then_http_response(error_rule_response(404, "NoSuchEntity"));
        let create = mock!(aws_sdk_iam::Client::create_login_profile)
            .match_requests(|i| {
                i.password() == Some("Sup3r-secret-pw!")
                    && i.password_reset_required() == Some(true)
            })
            .then_output(|| CreateLoginProfileOutput::builder().build());
        let get = mock!(aws_sdk_iam::Client::get_login_profile).then_output(|| {
            GetLoginProfileOutput::builder()
                .login_profile(
                    SdkLoginProfile::builder()
                        .user_name("alice")
                        .create_date(aws_smithy_types::DateTime::from_secs(1_700_000_000))
                        .password_reset_required(true)
                        .build()
                        .unwrap(),
                )
                .build()
        });
        let update = mock!(aws_sdk_iam::Client::update_login_profile)
            .match_requests(|i| {
                i.password().is_none() && i.password_reset_required() == Some(false)
            })
            .then_output(|| UpdateLoginProfileOutput::builder().build());
        let delete = mock!(aws_sdk_iam::Client::delete_login_profile)
            .then_output(|| DeleteLoginProfileOutput::builder().build());
        let iam = mock_iam(&[&none, &create, &get, &update, &delete]);
        assert_eq!(iam.get_login_profile("alice").await.unwrap(), None);
        let password = SecretString::from("Sup3r-secret-pw!");
        iam.create_login_profile("alice", &password, true)
            .await
            .unwrap();
        let profile = iam.get_login_profile("alice").await.unwrap().unwrap();
        assert!(profile.password_reset_required);
        iam.update_login_profile("alice", None, Some(false))
            .await
            .unwrap();
        assert_eq!(
            iam.update_login_profile("alice", None, None)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
        iam.delete_login_profile("alice", "alice").await.unwrap();
        assert_eq!(
            iam.create_login_profile("alice", &SecretString::from(""), false)
                .await
                .unwrap_err()
                .kind(),
            ErrorKind::Usage
        );
    }

    #[tokio::test]
    async fn details_report_each_part_on_its_own() {
        let get = mock!(aws_sdk_iam::Client::get_user)
            .then_output(|| GetUserOutput::builder().user(user("alice")).build());
        let attached = mock!(aws_sdk_iam::Client::list_attached_user_policies)
            .then_http_response(error_rule_response(403, "AccessDenied"));
        let inline = mock!(aws_sdk_iam::Client::list_user_policies).then_output(|| {
            ListUserPoliciesOutput::builder()
                .policy_names("p1")
                .is_truncated(false)
                .build()
                .unwrap()
        });
        let keys = mock!(aws_sdk_iam::Client::list_access_keys).then_output(|| {
            ListAccessKeysOutput::builder()
                .set_access_key_metadata(Some(Vec::new()))
                .is_truncated(false)
                .build()
                .unwrap()
        });
        let profile = mock!(aws_sdk_iam::Client::get_login_profile)
            .then_http_response(error_rule_response(404, "NoSuchEntity"));
        let groups = mock!(aws_sdk_iam::Client::list_groups_for_user).then_output(|| {
            ListGroupsForUserOutput::builder()
                .set_groups(Some(Vec::new()))
                .is_truncated(false)
                .build()
                .unwrap()
        });
        // Rules are matched by operation, so the order between them is free.
        let iam = mock_iam_with(
            RuleMode::MatchAny,
            &[&get, &attached, &inline, &keys, &profile, &groups],
        );
        let details = iam.user_details("alice").await.unwrap();
        assert_eq!(details.user.name, "alice");
        assert!(matches!(details.managed_policies, Section::Failed { .. }));
        assert_eq!(
            details.inline_policies,
            Section::Loaded {
                value: vec!["p1".to_owned()]
            }
        );
        assert_eq!(details.login_profile, Section::Loaded { value: None });
    }

    #[test]
    fn the_secret_never_shows_in_debug_or_errors() {
        let key = NewAccessKey {
            user_name: "alice".to_owned(),
            access_key_id: KEY_ID.to_owned(),
            secret_access_key: SecretString::from(SECRET),
            created: None,
        };
        let debug = format!("{key:?} {:#?}", key);
        assert!(!debug.contains(SECRET), "{debug}");
        assert!(debug.contains("<redacted>"));
        let json = serde_json::to_string(&key).unwrap();
        assert!(json.contains(SECRET), "the one-time display needs it");
        let password = SecretString::from("Sup3r-secret-pw!");
        assert!(!format!("{password:?}").contains("Sup3r"));
    }

    #[tokio::test]
    async fn the_secret_stays_out_of_the_logs() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone, Default)]
        struct Capture(Arc<Mutex<String>>);
        impl tracing::field::Visit for Capture {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
                if let Ok(mut text) = self.0.lock() {
                    text.push_str(&format!("{}={value:?} ", field.name()));
                }
            }
        }
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                event.record(&mut self.clone());
            }
        }
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        let key = mock!(aws_sdk_iam::Client::create_access_key).then_output(|| {
            CreateAccessKeyOutput::builder()
                .access_key(
                    AccessKey::builder()
                        .user_name("alice")
                        .access_key_id(KEY_ID)
                        .status(StatusType::Active)
                        .secret_access_key(SECRET)
                        .build()
                        .unwrap(),
                )
                .build()
        });
        let iam = mock_iam(&[&key]);
        let _guard = tracing::subscriber::set_default(subscriber);
        let created = iam.create_access_key("alice").await.unwrap();
        drop(created);
        let logged = capture.0.lock().unwrap().clone();
        assert!(!logged.contains(SECRET), "{logged}");
    }

    #[tokio::test]
    async fn a_custom_endpoint_has_no_iam() {
        let custom = account(Some("https://s3.example.test"));
        assert!(!iam_supported(&custom));
        let error = iam_config(&custom, &credentials(), &ClientOptions::default()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Usage);
        assert_eq!(error.to_string(), NOT_AVAILABLE);
        assert!(iam_supported(&account(None)));
    }

    #[tokio::test]
    async fn service_errors_are_classified() {
        for (status, code, kind) in [
            (404_u16, "NoSuchEntity", ErrorKind::NotFound),
            (409, "EntityAlreadyExists", ErrorKind::Conflict),
            (409, "DeleteConflict", ErrorKind::Conflict),
            (403, "AccessDenied", ErrorKind::Auth),
        ] {
            let rule = mock!(aws_sdk_iam::Client::create_user)
                .then_http_response(error_rule_response(status, code));
            let iam = mock_iam(&[&rule]);
            let error = iam.create_user("alice", None).await.unwrap_err();
            assert_eq!(error.kind(), kind, "{code}");
            assert!(error.to_string().starts_with("IAM CreateUser"), "{error}");
            assert_eq!(rule.num_calls(), 1, "no retries on {code}");
        }
    }

    #[test]
    fn names_and_paths_are_checked() {
        assert!(validate_user_name("a.b-c_d+e=f,g@h").is_ok());
        for bad in ["", "has space", &"x".repeat(65), "semi;colon"] {
            assert!(validate_user_name(bad).is_err(), "{bad}");
        }
        assert!(validate_path("/").is_ok());
        assert!(validate_path("/a/b/").is_ok());
        for bad in ["", "a/", "/a", "//", "/a//b/"] {
            assert!(validate_path(bad).is_err(), "{bad}");
        }
        assert!(validate_policy_arn(ARN).is_ok());
        assert!(validate_policy_arn("ReadOnlyAccess").is_err());
    }
}
