//! `bb account add|list|remove|default|import-aws|sso-login`: stored
//! accounts.
//!
//! Secrets are never command-line arguments (they would end up in the shell
//! history and in the process list): the secret access key comes from
//! standard input (`--secret-stdin`), from `BB_SECRET_ACCESS_KEY`, or from a
//! prompt on the terminal without echo. Accounts that get their credentials
//! from another source (`--assume-role`, `--sso`, `--instance-role`, AWS
//! profiles) have no secret at all.

use std::io::BufRead;

use bucketbridge_core::model::{
    Account, AccountInput, CredentialSource, DEFAULT_ROLE_DURATION_SECS, DEFAULT_ROLE_SESSION_NAME,
};
use bucketbridge_core::secrets::SecretString;
use bucketbridge_core::session::SsoLoginPoll;
use bucketbridge_core::{Error, Result, Session};

use crate::cli::{
    AccountAddArgs, AccountCommand, AccountSelector, AccountSsoLoginArgs, GlobalArgs,
};
use crate::context::{Inputs, dirs, env, find_account, open_session, unlock};
use crate::i18n::{tr, trf};
use crate::output::{Failure, Output};
use crate::records::{
    AccountChangeRecord, AccountRecord, ImportSkippedRecord, SsoPromptRecord, SsoSignedInRecord,
};

/// Runs an `account` subcommand.
pub async fn run(
    global: &GlobalArgs,
    command: &AccountCommand,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    if global.account.is_some() || global.mfa_code.is_some() {
        return Err(Error::Usage(tr(
            "--account and --mfa-code do not apply to `bb account`; name the account as an argument",
        ))
        .into());
    }
    if !global.profile.is_empty() && !matches!(command, AccountCommand::ImportAws) {
        return Err(Error::Usage(tr(
            "--profile applies to `bb account import-aws` only; add a profile account with it",
        ))
        .into());
    }
    let dirs = dirs(global, &inputs.env)?;
    let session = open_session(dirs.as_ref(), &inputs.secrets)?;
    match command {
        AccountCommand::Add(args) => add(&session, args, inputs, out).await,
        AccountCommand::List => list(&session, out).await,
        AccountCommand::Remove(selector) => remove(&session, selector, out).await,
        AccountCommand::Default(selector) => set_default(&session, selector, out).await,
        AccountCommand::ImportAws => import_aws(&session, &global.profile, out).await,
        AccountCommand::SsoLogin(args) => sso_login(&session, args, inputs, out).await,
    }
}

async fn add(
    session: &Session,
    args: &AccountAddArgs,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let mut input = AccountInput::default();
    input.label.clone_from(&args.label);
    input.region.clone_from(&args.region);
    input.endpoint.clone_from(&args.endpoint);
    input.path_style = args.path_style;
    input.request_payer = args.request_payer;
    if args.instance_role {
        if !args.no_probe {
            let found = session.probe_instance_role().await?;
            out.notice(&trf(
                "found the role of this machine (access key {key})",
                &[("key", &found.access_key_id)],
            ));
        }
        input.source = CredentialSource::InstanceRole;
    } else if let Some(role_arn) = &args.assume_role {
        input.source = role_source(session, args, role_arn).await?;
    } else if args.sso {
        input.source = CredentialSource::Sso {
            start_url: required(&args.sso_start_url, "--sso-start-url")?,
            sso_region: required(&args.sso_region, "--sso-region")?,
            account_id: required(&args.sso_account_id, "--sso-account-id")?,
            role_name: required(&args.sso_role_name, "--sso-role-name")?,
            sso_session: args.sso_session.clone(),
        };
    } else {
        // Storing a secret under a master password needs the key.
        unlock(session, inputs).await?;
        let (secret, session_token) = read_secrets(args, inputs)?;
        input.access_key_id = required(&args.access_key_id, "--access-key-id")?;
        input.secret_access_key = secret.expose().to_owned();
        input.session_token = session_token
            .as_ref()
            .map(|token| token.expose().to_owned());
        input.mfa_serial.clone_from(&args.mfa_serial);
        drop(secret);
        drop(session_token);
    }
    let account = session.add_account(input).await?;
    let list = session.list_accounts().await?;
    let record = AccountRecord {
        is_default: list.default_account_id.as_deref() == Some(account.id.as_str()),
        account,
    };
    out.change(&record, || {
        trf(
            "added account {label} ({id})",
            &[
                ("label", record.account.label.as_str()),
                ("id", record.account.id.as_str()),
            ],
        )
    })
}

fn required(value: &Option<String>, flag: &str) -> Result<String> {    value.clone().ok_or_else(|| {
        Error::Usage(trf("{flag} is required", &[("flag", flag)]))
    })
}

/// The assumed-role source of `bb account add --assume-role`.
async fn role_source(
    session: &Session,
    args: &AccountAddArgs,
    role_arn: &str,
) -> Result<CredentialSource> {
    let selector = required(&args.source_account, "--source-account")?;
    let list = session.list_accounts().await?;
    let source = find_account(&list.accounts, &selector).ok_or_else(|| {
        Error::NotFound(trf(
            "account '{selector}' not found; `bb account list` shows the stored accounts",
            &[("selector", selector.as_str())],
        ))
    })?;
    let duration_secs = match args.duration {
        Some(duration) => u32::try_from(duration.as_secs())
            .map_err(|_| Error::Usage(tr("--duration is too long")))?,
        None => DEFAULT_ROLE_DURATION_SECS,
    };
    Ok(CredentialSource::AssumeRole {
        source_account_id: source.id.clone(),
        role_arn: role_arn.to_owned(),
        session_name: args
            .session_name
            .clone()
            .unwrap_or_else(|| DEFAULT_ROLE_SESSION_NAME.to_owned()),
        duration_secs,
        external_id: args.external_id.clone(),
        mfa_serial: args.mfa_serial.clone(),
    })
}

/// The secret access key and an optional session token.
fn read_secrets(
    args: &AccountAddArgs,
    inputs: &Inputs,
) -> Result<(SecretString, Option<SecretString>)> {
    if args.secret_stdin {
        let mut stdin = inputs
            .stdin
            .lock()
            .map_err(|_| Error::Internal(tr("standard input is unavailable")))?;
        let secret = read_line(&mut **stdin)?.ok_or_else(|| {
            Error::Usage(tr(
                "--secret-stdin: standard input is empty; expected the secret access key on the first line",
            ))
        })?;
        let token = read_line(&mut **stdin)?;
        return Ok((secret, token));
    }
    let secret = match inputs.env.get(env::SECRET_ACCESS_KEY)? {
        Some(secret) => SecretString::new(secret),
        None => match inputs.console.read_secret(&tr("Secret access key: ")) {
            Ok(Some(secret)) if !secret.is_empty() => SecretString::new(secret),
            _ => {
                return Err(Error::Usage(trf(
                    "no secret access key: use --secret-stdin, set {env}, or run bb in a terminal",
                    &[("env", env::SECRET_ACCESS_KEY)],
                )));
            }
        },
    };
    let token = inputs.env.get(env::SESSION_TOKEN)?.map(SecretString::new);
    Ok((secret, token))
}

/// One line without its line ending; `None` at the end of input or for an
/// empty line.
fn read_line(reader: &mut dyn BufRead) -> Result<Option<SecretString>> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| Error::io("reading standard input", err))?;
    let secret = SecretString::new(line.trim_end_matches(['\r', '\n']).to_owned());
    drop(line);
    Ok((!secret.is_empty()).then_some(secret))
}

async fn list(session: &Session, out: &mut Output) -> std::result::Result<(), Failure> {
    let list = session.list_accounts().await?;
    for account in list.accounts {
        let record = AccountRecord {
            is_default: list.default_account_id.as_deref() == Some(account.id.as_str()),
            account,
        };
        out.row(&record, || {
            format!(
                "{} {:<24}  {:<14}  {:<30}  {:<14}  {}",
                if record.is_default { "*" } else { " " },
                record.account.label,
                record.account.region,
                record.account.endpoint.as_deref().unwrap_or("-"),
                source_label(&record.account),
                record.account.id
            )
        })?;
    }
    Ok(())
}

/// The credential source of an account as a short word for the list.
fn source_label(account: &Account) -> &'static str {
    match account.source {
        CredentialSource::StaticKeys => "keys",
        CredentialSource::AwsProfile { .. } => "aws-profile",
        CredentialSource::AssumeRole { .. } => "assume-role",
        CredentialSource::Sso { .. } => "sso",
        CredentialSource::InstanceRole => "instance-role",
    }
}

/// `bb account import-aws`: links profiles of the AWS files as accounts.
async fn import_aws(
    session: &Session,
    named: &[String],
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let explicit = !named.is_empty();
    let names: Vec<String> = if explicit {
        named.to_vec()
    } else {
        session
            .list_aws_profiles()
            .await?
            .into_iter()
            .filter(|entry| entry.profile.importable && entry.account_id.is_none())
            .map(|entry| entry.profile.name)
            .collect()
    };
    if names.is_empty() {
        out.notice(&tr("no AWS profile to import"));
        return Ok(());
    }
    let outcome = session.import_aws_profiles(&names).await?;
    let list = session.list_accounts().await?;
    for account in &outcome.imported {
        let record = AccountRecord {
            is_default: list.default_account_id.as_deref() == Some(account.id.as_str()),
            account: account.clone(),
        };
        out.change(&record, || {
            trf(
                "imported profile as account {label} ({id})",
                &[
                    ("label", record.account.label.as_str()),
                    ("id", record.account.id.as_str()),
                ],
            )
        })?;
    }
    for skipped in &outcome.skipped {
        let record = ImportSkippedRecord {
            profile: skipped.profile.clone(),
            reason: skipped.reason.clone(),
            skipped: true,
        };
        out.row(&record, || {
            trf(
                "skipped profile {profile}: {reason}",
                &[
                    ("profile", skipped.profile.as_str()),
                    ("reason", skipped.reason.as_str()),
                ],
            )
        })?;
    }
    if outcome.imported.is_empty() && explicit {
        return Err(Error::Usage(tr("no profile was imported")).into());
    }
    Ok(())
}

/// `bb account sso-login`: the device authorization flow.
async fn sso_login(
    session: &Session,
    args: &AccountSsoLoginArgs,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let (start_url, region, sso_session) = portal_of(session, args).await?;
    let started = session
        .sso_start_login(&start_url, &region, sso_session.as_deref())
        .await?;
    let target = started
        .verification_uri_complete
        .clone()
        .unwrap_or_else(|| started.verification_uri.clone());
    let record = SsoPromptRecord {
        user_code: started.user_code.clone(),
        verification_uri: started.verification_uri.clone(),
        verification_uri_complete: started.verification_uri_complete.clone(),
        expires_in_secs: started.expires_in_secs,
    };
    out.row(&record, || {
        trf(
            "Open {url} and confirm the code {code}",
            &[
                ("url", record.verification_uri.to_string().as_str()),
                ("code", record.user_code.as_str()),
            ],
        )
    })?;
    if !args.no_browser
        && !out.json()
        && inputs.console.stderr_is_terminal()
        && let Err(reason) = open_in_browser(&target)
    {
        out.notice(&trf(
            "could not open the browser: {reason}",
            &[("reason", &reason)],
        ));
    }
    let mut interval = started.interval_secs.max(1);
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        match session.sso_poll_login(&started.login_id).await? {
            SsoLoginPoll::Pending { interval_secs } => interval = interval_secs.max(1),
            SsoLoginPoll::Complete { expires_at } => {
                let done = SsoSignedInRecord {
                    start_url: start_url.clone(),
                    sso_region: region.clone(),
                    expires_at,
                };
                return out.change(&done, || {
                    trf(
                        "signed in to {url} until {time}",
                        &[
                            ("url", done.start_url.as_str()),
                            ("time", done.expires_at.to_string().as_str()),
                        ],
                    )
                });
            }
        }
    }
}

/// The portal to sign in to: named by the account argument or the options,
/// else the portal of the only stored SSO account.
async fn portal_of(
    session: &Session,
    args: &AccountSsoLoginArgs,
) -> Result<(String, String, Option<String>)> {
    let list = session.list_accounts().await?;
    let portal_of = |account: &Account| match &account.source {
        CredentialSource::Sso {
            start_url,
            sso_region,
            sso_session,
            ..
        } => Some((start_url.clone(), sso_region.clone(), sso_session.clone())),
        _ => None,
    };
    if let Some(selector) = &args.account_label {
        let account = find_account(&list.accounts, selector).ok_or_else(|| {
            Error::NotFound(trf(
                "account '{selector}' not found; `bb account list` shows the stored accounts",
                &[("selector", selector)],
            ))
        })?;
        return portal_of(account).ok_or_else(|| {
            Error::Usage(trf(
                "account '{selector}' does not use IAM Identity Center",
                &[("selector", selector)],
            ))
        });
    }
    if let (Some(url), Some(region)) = (&args.start_url, &args.sso_region) {
        return Ok((url.clone(), region.clone(), args.sso_session.clone()));
    }
    let mut portals: Vec<_> = list.accounts.iter().filter_map(portal_of).collect();
    portals.dedup();
    match portals.as_slice() {
        [only] if args.start_url.is_none() && args.sso_region.is_none() => Ok(only.clone()),
        _ => Err(Error::Usage(tr(
            "name the portal with --start-url and --sso-region, or an SSO account",
        ))),
    }
}

/// Opens `url` with the platform's default handler.
fn open_in_browser(url: &str) -> std::result::Result<(), String> {
    if !url.starts_with("https://") {
        return Err("only https URLs are opened".into());
    }
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn remove(
    session: &Session,
    selector: &AccountSelector,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let record = select(session, selector).await?;
    session.remove_account(&record.id).await?;
    out.change(&record, || {
        trf(
            "removed account {label} ({id})",
            &[("label", &record.label), ("id", &record.id)],
        )
    })
}

async fn set_default(
    session: &Session,
    selector: &AccountSelector,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let record = select(session, selector).await?;
    session.set_default_account(&record.id).await?;
    out.change(&record, || {
        trf(
            "default account is now {label} ({id})",
            &[("label", &record.label), ("id", &record.id)],
        )
    })
}

async fn select(session: &Session, selector: &AccountSelector) -> Result<AccountChangeRecord> {
    let list = session.list_accounts().await?;
    let account = find_account(&list.accounts, &selector.label_or_id).ok_or_else(|| {
        Error::NotFound(trf(
            "account '{selector}' not found; `bb account list` shows the stored accounts",
            &[("selector", selector.label_or_id.as_str())],
        ))
    })?;
    Ok(AccountChangeRecord {
        id: account.id.clone(),
        label: account.label.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_without_line_endings() {
        let mut input: &[u8] = b"secret\r\ntoken\n\nrest";
        assert_eq!(read_line(&mut input).unwrap().unwrap().expose(), "secret");
        assert_eq!(read_line(&mut input).unwrap().unwrap().expose(), "token");
        assert!(read_line(&mut input).unwrap().is_none());
        assert_eq!(read_line(&mut input).unwrap().unwrap().expose(), "rest");
        assert!(read_line(&mut input).unwrap().is_none());
    }
}
