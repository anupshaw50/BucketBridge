//! `bb iam ...`: IAM users, access keys, managed and inline policies and
//! console passwords, over `bucketbridge_core::iam`.
//!
//! The commands are for AWS accounts: an account with a custom endpoint
//! fails with `usage`. Destructive commands need `--yes`; the core still
//! gets the typed confirmation (the name or key ID) it demands. Secrets
//! (the secret of a new access key, a generated password) are printed once,
//! with a warning on standard error, and are a field of the `--json` record;
//! they are data, so `--quiet` does not hide them. A password to set is read
//! from standard input (`--password-stdin`), generated (`--generate`) or
//! asked on the terminal, never a command-line argument.

use std::io::{BufRead, Read};
use std::path::Path;

use bucketbridge_core::bucketcfg::{PolicyIssue, Section};
use bucketbridge_core::iam::{
    self, AccessKeyInfo, Iam, InlinePolicy, KeyStatus, LoginProfile, ManagedPolicy, PolicyScope,
    UserDetails,
};
use bucketbridge_core::secrets::SecretString;
use bucketbridge_core::{Error, Result};
use serde::Serialize;

use super::timestamp;
use crate::cli::{
    GlobalArgs, IamCommand, IamInlineCommand, IamKeysCommand, IamLoginProfileCommand,
    IamPolicyCommand, IamScopeArg, IamUserCommand,
};
use crate::context::{Inputs, iam_client};
use crate::i18n::{tr, trf};
use crate::output::{Failure, Output};

/// Characters of a generated password.
const GENERATED_PASSWORD_CHARS: usize = 24;

const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const DIGITS: &[u8] = b"23456789";
const SYMBOLS: &[u8] = b"!@#$%^&*()_+-=";

/// Runs an `iam` subcommand.
pub async fn run(
    global: &GlobalArgs,
    command: &IamCommand,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    // Commands that need no account, and usage errors, come first.
    match command {
        IamCommand::ValidatePolicy(args) => return validate_policy(&args.file, inputs, out),
        IamCommand::FormatPolicy(args) => return format_policy(&args.file, inputs, out),
        _ => check(command)?,
    }
    let iam = iam_client(global, inputs).await?;
    match command {
        IamCommand::Users(args) => users(&iam, args.path_prefix.as_deref(), out).await,
        IamCommand::User(command) => user(&iam, command, out).await,
        IamCommand::Keys(command) => keys(&iam, command, out).await,
        IamCommand::Policy(command) => policy(&iam, command, out).await,
        IamCommand::Inline(command) => inline(&iam, command, inputs, out).await,
        IamCommand::LoginProfile(command) => login_profile(&iam, command, inputs, out).await,
        IamCommand::ValidatePolicy(_) | IamCommand::FormatPolicy(_) => Ok(()),
    }
}

/// Usage checks that need no account: a destructive command needs `--yes`.
fn check(command: &IamCommand) -> Result<()> {
    let (yes, what) = match command {
        IamCommand::User(IamUserCommand::Delete(args)) => (
            args.yes,
            trf(
                "deleting user {name} and everything it owns",
                &[("name", &args.name)],
            ),
        ),
        IamCommand::Keys(IamKeysCommand::Delete(args)) => (
            args.yes,
            trf("deleting access key {key_id}", &[("key_id", &args.key_id)]),
        ),
        IamCommand::Inline(IamInlineCommand::Delete(args)) => (
            args.yes,
            trf("deleting the inline policy {policy}", &[("policy", &args.policy)]),
        ),
        IamCommand::LoginProfile(IamLoginProfileCommand::Delete(args)) => (
            args.yes,
            trf("removing the console password of {name}", &[("name", &args.name)]),
        ),
        _ => return Ok(()),
    };
    if yes {
        Ok(())
    } else {
        Err(Error::Usage(trf("{what} needs --yes", &[("what", &what)])))
    }
}

// ---- users ----

async fn users(
    iam: &Iam,
    prefix: Option<&str>,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    for user in iam.list_users(prefix).await? {
        out.row(&user, || {
            format!(
                "{}\t{}\t{}\t{}",
                user.name,
                user.path,
                timestamp(user.created),
                user.arn
            )
        })?;
    }
    Ok(())
}

async fn user(
    iam: &Iam,
    command: &IamUserCommand,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    match command {
        IamUserCommand::Show(args) => {
            let details = iam.user_details(&args.name).await?;
            out.row(&details, || details_text(&details))
        }
        IamUserCommand::Create(args) => {
            let user = iam.create_user(&args.name, args.path.as_deref()).await?;
            out.change(&user, || {
                trf(
                    "created user {name} ({arn})",
                    &[("name", user.name.as_str()), ("arn", user.arn.as_str())],
                )
            })
        }
        IamUserCommand::Update(args) => {
            let user = iam
                .update_user(
                    &args.name,
                    args.new_name.as_deref(),
                    args.new_path.as_deref(),
                )
                .await?;
            out.change(&user, || {
                trf(
                    "updated user {name} ({arn})",
                    &[("name", user.name.as_str()), ("arn", user.arn.as_str())],
                )
            })
        }
        IamUserCommand::Delete(args) => {
            let outcome = iam.delete_user(&args.name, &args.name).await?;
            let mut first_error = None;
            for step in &outcome.steps {
                if let Some(error) = &step.error {
                    first_error.get_or_insert_with(|| error.clone());
                    let line = trf(
                        "bb: failed to remove {kind} {target}: {message}",
                        &[
                            ("kind", step.kind.as_str()),
                            ("target", step.target.as_str()),
                            ("message", error.message.as_str()),
                        ],
                    );
                    out.notice(&line);
                }
            }
            out.change(&outcome, || {
                if outcome.deleted {
                    trf("deleted user {name}", &[("name", outcome.user_name.as_str())])
                } else {
                    trf(
                        "did not delete user {name}",
                        &[("name", outcome.user_name.as_str())],
                    )
                }
            })?;
            match first_error {
                Some(error) if !outcome.deleted => Err(Error::Remote(trf(
                    "user {name} was not deleted: {message}",
                    &[
                        ("name", outcome.user_name.as_str()),
                        ("message", error.message.as_str()),
                    ],
                ))
                .into()),
                _ => Ok(()),
            }
        }
    }
}

fn section_lines<T>(
    title: &str,
    section: &Section<T>,
    lines: impl FnOnce(&T) -> Vec<String>,
) -> Vec<String> {
    let mut all = vec![trf("{title}:", &[("title", tr(title).as_str())])];
    match section {
        Section::Loaded { value } => {
            let rows = lines(value);
            if rows.is_empty() {
                all.push(tr("  (none)"));
            } else {
                all.extend(rows.into_iter().map(|row| format!("  {row}")));
            }
        }
        Section::Unsupported { message } => all.push(trf(
            "  (not available: {message})",
            &[("message", message)],
        )),
        Section::Failed { error } => all.push(trf(
            "  (could not be read: {message})",
            &[("message", error.message.as_str())],
        )),
    }
    all
}

fn details_text(details: &UserDetails) -> String {
    let user = &details.user;
    let mut lines = vec![
        trf("user: {name}", &[("name", user.name.as_str())]),
        trf("arn: {arn}", &[("arn", user.arn.as_str())]),
        trf("path: {path}", &[("path", user.path.as_str())]),
        trf("created: {time}", &[("time", timestamp(user.created).as_str())]),
        trf("password last used: {time}", &[("time", timestamp(user.password_last_used).as_str())]),
    ];
    lines.extend(section_lines(
        "managed policies",
        &details.managed_policies,
        |policies| policies.iter().map(|p| p.arn.clone()).collect(),
    ));
    lines.extend(section_lines(
        "inline policies",
        &details.inline_policies,
        Clone::clone,
    ));
    lines.extend(section_lines("access keys", &details.access_keys, |keys| {
        keys.iter()
            .map(|k| {
                format!(
                    "{}  {}  {}",
                    k.access_key_id,
                    status_text(k.status),
                    timestamp(k.created)
                )
            })
            .collect()
    }));
    lines.extend(section_lines(
        "console password",
        &details.login_profile,
        |profile| match profile {
            Some(profile) => vec![login_text(profile)],
            None => vec!["none".to_owned()],
        },
    ));
    lines.extend(section_lines("groups", &details.groups, |groups| {
        groups.iter().map(|g| g.name.clone()).collect()
    }));
    lines.join("\n")
}

fn status_text(status: KeyStatus) -> &'static str {
    match status {
        KeyStatus::Active => "active",
        KeyStatus::Inactive => "inactive",
    }
}

fn login_text(profile: &LoginProfile) -> String {
    trf(
        "set {time}{reset}",
        &[
            ("time", timestamp(profile.created).as_str()),
            (
                "reset",
                tr(if profile.password_reset_required {
                    ", must be changed at the next sign-in"
                } else {
                    ""
                })
                .as_str(),
            ),
        ],
    )
}

// ---- access keys ----

/// `bb iam keys list`
#[derive(Serialize)]
struct KeyRecord<'a> {
    user: &'a str,
    #[serde(flatten)]
    key: &'a AccessKeyInfo,
}

/// `bb iam keys activate|deactivate|delete`
#[derive(Serialize)]
struct KeyChangeRecord<'a> {
    user: &'a str,
    access_key_id: &'a str,
    /// `active`, `inactive` or `deleted`.
    status: &'static str,
}

async fn keys(
    iam: &Iam,
    command: &IamKeysCommand,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    match command {
        IamKeysCommand::List(args) => {
            for key in iam.list_access_keys(&args.name).await? {
                let record = KeyRecord {
                    user: &args.name,
                    key: &key,
                };
                out.row(&record, || {
                    format!(
                        "{}\t{}\t{}",
                        key.access_key_id,
                        status_text(key.status),
                        timestamp(key.created)
                    )
                })?;
            }
            Ok(())
        }
        IamKeysCommand::Create(args) => {
            let key = iam.create_access_key(&args.name).await?;
            out.notice(&tr(
                "bb: warning: the secret access key is shown only now; IAM cannot show it again",
            ));
            out.row(&key, || {
                trf(
                    "user: {user}\naccess key ID: {key_id}\nsecret access key: {secret}",
                    &[
                        ("user", key.user_name.to_string().as_str()),
                        ("key_id", key.access_key_id.to_string().as_str()),
                        ("secret", key.secret_access_key.expose()),
                    ],
                )
            })
        }
        IamKeysCommand::Activate(args) => {
            set_active(iam, &args.name, &args.key_id, true, out).await
        }
        IamKeysCommand::Deactivate(args) => {
            set_active(iam, &args.name, &args.key_id, false, out).await
        }
        IamKeysCommand::Delete(args) => {
            iam.delete_access_key(&args.name, &args.key_id, &args.key_id)
                .await?;
            let record = KeyChangeRecord {
                user: &args.name,
                access_key_id: &args.key_id,
                status: "deleted",
            };
            out.change(&record, || {
                trf(
                    "deleted access key {key_id} of {user}",
                    &[("key_id", &args.key_id), ("user", &args.name)],
                )
            })
        }
    }
}

async fn set_active(
    iam: &Iam,
    user: &str,
    key_id: &str,
    active: bool,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    iam.set_access_key_active(user, key_id, active).await?;
    let status = if active { "active" } else { "inactive" };
    let record = KeyChangeRecord {
        user,
        access_key_id: key_id,
        status,
    };
    out.change(&record, || {
        trf(
            if active {
                "activated access key {key_id} of {user}"
            } else {
                "deactivated access key {key_id} of {user}"
            },
            &[("key_id", key_id), ("user", user)],
        )
    })
}

// ---- managed policies ----

/// `bb iam policy attach|detach`
#[derive(Serialize)]
struct AttachRecord<'a> {
    user: &'a str,
    policy_arn: &'a str,
    attached: bool,
}

fn policy_line(policy: &ManagedPolicy) -> String {
    match policy.attachment_count {
        Some(count) => format!("{}\t{}\t{count}", policy.name, policy.arn),
        None => format!("{}\t{}", policy.name, policy.arn),
    }
}

async fn policy(
    iam: &Iam,
    command: &IamPolicyCommand,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    match command {
        IamPolicyCommand::ListManaged(args) => {
            let scope = match args.scope {
                IamScopeArg::Local => PolicyScope::Local,
                IamScopeArg::Aws => PolicyScope::Aws,
                IamScopeArg::All => PolicyScope::All,
            };
            for policy in iam.list_managed_policies(scope).await? {
                out.row(&policy, || policy_line(&policy))?;
            }
            Ok(())
        }
        IamPolicyCommand::List(args) => {
            for policy in iam.list_attached_policies(&args.name).await? {
                out.row(&policy, || policy_line(&policy))?;
            }
            Ok(())
        }
        IamPolicyCommand::Attach(args) => {
            iam.attach_user_policy(&args.name, &args.policy_arn).await?;
            let record = AttachRecord {
                user: &args.name,
                policy_arn: &args.policy_arn,
                attached: true,
            };
            out.change(&record, || {
                trf(
                    "attached {policy} to {user}",
                    &[("policy", &args.policy_arn), ("user", &args.name)],
                )
            })
        }
        IamPolicyCommand::Detach(args) => {
            iam.detach_user_policy(&args.name, &args.policy_arn).await?;
            let record = AttachRecord {
                user: &args.name,
                policy_arn: &args.policy_arn,
                attached: false,
            };
            out.change(&record, || {
                trf(
                    "detached {policy} from {user}",
                    &[("policy", &args.policy_arn), ("user", &args.name)],
                )
            })
        }
    }
}

// ---- inline policies ----

/// `bb iam inline list`
#[derive(Serialize)]
struct InlineNameRecord<'a> {
    user: &'a str,
    policy: &'a str,
}

/// `bb iam inline get`
#[derive(Serialize)]
struct InlineGetRecord<'a> {
    user: &'a str,
    #[serde(flatten)]
    policy: &'a InlinePolicy,
}

/// `bb iam inline put|delete`
#[derive(Serialize)]
struct InlineChangeRecord<'a> {
    user: &'a str,
    policy: &'a str,
    /// `put` or `deleted`.
    action: &'static str,
}

async fn inline(
    iam: &Iam,
    command: &IamInlineCommand,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    match command {
        IamInlineCommand::List(args) => {
            for name in iam.list_inline_policies(&args.name).await? {
                let record = InlineNameRecord {
                    user: &args.name,
                    policy: &name,
                };
                out.row(&record, || name.clone())?;
            }
            Ok(())
        }
        IamInlineCommand::Get(args) => {
            let policy = iam.get_inline_policy(&args.name, &args.policy).await?;
            let record = InlineGetRecord {
                user: &args.name,
                policy: &policy,
            };
            out.row(&record, || policy.document.clone())
        }
        IamInlineCommand::Put(args) => {
            let document = read_document(&args.file, inputs)?;
            iam.put_inline_policy(&args.name, &args.policy, &document)
                .await?;
            let record = InlineChangeRecord {
                user: &args.name,
                policy: &args.policy,
                action: "put",
            };
            out.change(&record, || {
                trf(
                    "saved inline policy {policy} of {user}",
                    &[("policy", &args.policy), ("user", &args.name)],
                )
            })
        }
        IamInlineCommand::Delete(args) => {
            iam.delete_inline_policy(&args.name, &args.policy, &args.policy)
                .await?;
            let record = InlineChangeRecord {
                user: &args.name,
                policy: &args.policy,
                action: "deleted",
            };
            out.change(&record, || {
                trf(
                    "deleted inline policy {policy} of {user}",
                    &[("policy", &args.policy), ("user", &args.name)],
                )
            })
        }
    }
}

// ---- console passwords ----

/// `bb iam login-profile show`
#[derive(Serialize)]
struct LoginShowRecord<'a> {
    user: &'a str,
    exists: bool,
    created: Option<chrono::DateTime<chrono::Utc>>,
    password_reset_required: Option<bool>,
}

/// `bb iam login-profile create|update|delete`
#[derive(Serialize)]
struct LoginChangeRecord<'a> {
    user: &'a str,
    /// `created`, `updated` or `deleted`.
    action: &'static str,
    /// The generated password (`--generate`); `null` otherwise.
    password: Option<&'a str>,
}

async fn login_profile(
    iam: &Iam,
    command: &IamLoginProfileCommand,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    match command {
        IamLoginProfileCommand::Show(args) => {
            let profile = iam.get_login_profile(&args.name).await?;
            let record = LoginShowRecord {
                user: &args.name,
                exists: profile.is_some(),
                created: profile.as_ref().and_then(|p| p.created),
                password_reset_required: profile.as_ref().map(|p| p.password_reset_required),
            };
            out.row(&record, || match &profile {
                Some(profile) => {
                    trf("{name}: {what}", &[("name", &args.name), ("what", login_text(profile).as_str())])
                }
                None => trf("{name}: no console password", &[("name", &args.name)]),
            })
        }
        IamLoginProfileCommand::Create(args) => {
            let (password, generated) =
                password_for(args.password_stdin, args.generate, true, inputs)?
                    .ok_or_else(|| Error::Usage(tr("no password")))?;
            iam.create_login_profile(&args.name, &password, args.reset_required)
                .await?;
            report_password(&args.name, "created", &password, generated, out)
        }
        IamLoginProfileCommand::Update(args) => {
            let reset = match (args.reset_required, args.no_reset_required) {
                (true, _) => Some(true),
                (_, true) => Some(false),
                _ => None,
            };
            let new = password_for(args.password_stdin, args.generate, false, inputs)?;
            iam.update_login_profile(&args.name, new.as_ref().map(|(p, _)| p), reset)
                .await?;
            match new {
                Some((password, generated)) => {
                    report_password(&args.name, "updated", &password, generated, out)
                }
                None => {
                    let record = LoginChangeRecord {
                        user: &args.name,
                        action: "updated",
                        password: None,
                    };
                    out.change(&record, || {
                        trf(
                            "updated the console password settings of {user}",
                            &[("user", &args.name)],
                        )
                    })
                }
            }
        }
        IamLoginProfileCommand::Delete(args) => {
            iam.delete_login_profile(&args.name, &args.name).await?;
            let record = LoginChangeRecord {
                user: &args.name,
                action: "deleted",
                password: None,
            };
            out.change(&record, || {
                trf(
                    "removed the console password of {user}",
                    &[("user", &args.name)],
                )
            })
        }
    }
}

/// Prints the outcome of setting a password; a generated one is shown once.
fn report_password(
    user: &str,
    action: &'static str,
    password: &SecretString,
    generated: bool,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    if !generated {
        let record = LoginChangeRecord {
            user,
            action,
            password: None,
        };
        return out.change(&record, || {
            trf(
                "{verb} the console password of {user}",
                &[
                    ("verb", tr(match action {
                        "created" => "created",
                        "updated" => "updated",
                        _ => "removed",
                    }).as_str()),
                    ("user", user),
                ],
            )
        });
    }
    out.notice(&tr(
        "bb: warning: the generated password is shown only now; it cannot be read again",
    ));
    let record = LoginChangeRecord {
        user,
        action,
        password: Some(password.expose()),
    };
    out.row(&record, || {
        trf(
            "{verb} the console password of {user}\npassword: {password}",
            &[
                ("verb", tr(match action {
                    "created" => "created",
                    "updated" => "updated",
                    _ => "removed",
                }).as_str()),
                ("user", user),
                ("password", password.expose()),
            ],
        )
    })
}

/// The password to set and whether it was generated. `None` when no source
/// was chosen and `required` is false (an update of the settings only).
fn password_for(
    from_stdin: bool,
    generate: bool,
    required: bool,
    inputs: &Inputs,
) -> Result<Option<(SecretString, bool)>> {
    if generate {
        return Ok(Some((generate_password()?, true)));
    }
    if from_stdin {
        let mut stdin = inputs
            .stdin
            .lock()
            .map_err(|_| Error::Internal(tr("standard input is unavailable")))?;
        let mut line = String::new();
        stdin
            .read_line(&mut line)
            .map_err(|err| Error::io("reading standard input", err))?;
        let password = SecretString::new(line.trim_end_matches(['\r', '\n']).to_owned());
        drop(line);
        if password.is_empty() {
            return Err(Error::Usage(tr(
                "--password-stdin: standard input is empty; expected the password on the first line",
            )));
        }
        return Ok(Some((password, false)));
    }
    if !required {
        return Ok(None);
    }
    match inputs.console.read_secret(&tr("Console password: ")) {
        Ok(Some(password)) if !password.is_empty() => {
            Ok(Some((SecretString::new(password), false)))
        }
        _ => Err(Error::Usage(tr(
            "no password: use --password-stdin or --generate, or run bb in a terminal",
        ))),
    }
}

/// A random password with at least one upper-case letter, lower-case
/// letter, digit and symbol (IAM account policies usually ask for them).
fn generate_password() -> Result<SecretString> {
    let all: Vec<u8> = [UPPER, LOWER, DIGITS, SYMBOLS].concat();
    loop {
        let mut chars = Vec::with_capacity(GENERATED_PASSWORD_CHARS);
        while chars.len() < GENERATED_PASSWORD_CHARS {
            chars.push(pick(&all)?);
        }
        if [UPPER, LOWER, DIGITS, SYMBOLS]
            .iter()
            .all(|class| chars.iter().any(|c| class.contains(c)))
        {
            return Ok(SecretString::new(
                chars.into_iter().map(char::from).collect(),
            ));
        }
    }
}

/// One uniformly random byte of `alphabet` (rejection sampling).
fn pick(alphabet: &[u8]) -> Result<u8> {
    let limit = 256 - 256 % alphabet.len();
    loop {
        let mut byte = [0u8; 1];
        getrandom::fill(&mut byte)
            .map_err(|err| Error::Internal(format!("no random numbers: {err}")))?;
        if usize::from(byte[0]) < limit {
            return Ok(alphabet[usize::from(byte[0]) % alphabet.len()]);
        }
    }
}

// ---- policy documents ----

/// `bb iam validate-policy`
#[derive(Serialize)]
struct ValidateRecord<'a> {
    file: &'a str,
    valid: bool,
    issues: &'a [PolicyIssue],
}

/// `bb iam format-policy`
#[derive(Serialize)]
struct FormatRecord<'a> {
    file: &'a str,
    document: &'a str,
}

/// The text of a policy file; `-` is standard input.
fn read_document(file: &Path, inputs: &Inputs) -> Result<String> {
    if file == Path::new("-") {
        let mut stdin = inputs
            .stdin
            .lock()
            .map_err(|_| Error::Internal("standard input is unavailable".into()))?;
        let mut text = String::new();
        stdin
            .read_to_string(&mut text)
            .map_err(|err| Error::io("reading standard input", err))?;
        Ok(text)
    } else {
        std::fs::read_to_string(file)
            .map_err(|err| Error::io(format!("reading {}", file.display()), err))
    }
}

fn validate_policy(
    file: &Path,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let name = file.display().to_string();
    let issues = iam::check_user_policy(&read_document(file, inputs)?);
    let record = ValidateRecord {
        file: &name,
        valid: issues.is_empty(),
        issues: &issues,
    };
    out.row(&record, || {
        if issues.is_empty() {
            trf("{name}: valid", &[("name", &name)])
        } else {
            issues
                .iter()
                .map(|issue| {
                    if issue.path.is_empty() {
                        trf("{name}: {message}", &[("name", &name), ("message", issue.message.as_str())])
                    } else {
                        trf(
                            "{name}: {path}: {message}",
                            &[
                                ("name", &name),
                                ("path", issue.path.as_str()),
                                ("message", issue.message.as_str()),
                            ],
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    })?;
    if issues.is_empty() {
        Ok(())
    } else {
        Err(Error::Usage(trf(
            "{count} problem(s) found in {name}",
            &[
                ("count", issues.len().to_string().as_str()),
                ("name", &name),
            ],
        ))
        .into())
    }
}

fn format_policy(
    file: &Path,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let name = file.display().to_string();
    let document = iam::format_user_policy(&read_document(file, inputs)?)?;
    let record = FormatRecord {
        file: &name,
        document: &document,
    };
    out.row(&record, || document.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_passwords_have_every_class() {
        for _ in 0..50 {
            let password = generate_password().unwrap();
            let text = password.expose();
            assert_eq!(text.len(), GENERATED_PASSWORD_CHARS);
            assert!(text.bytes().any(|c| UPPER.contains(&c)));
            assert!(text.bytes().any(|c| LOWER.contains(&c)));
            assert!(text.bytes().any(|c| DIGITS.contains(&c)));
            assert!(text.bytes().any(|c| SYMBOLS.contains(&c)));
        }
    }

    #[test]
    fn picks_stay_in_the_alphabet() {
        for _ in 0..200 {
            assert!(DIGITS.contains(&pick(DIGITS).unwrap()));
        }
    }
}
