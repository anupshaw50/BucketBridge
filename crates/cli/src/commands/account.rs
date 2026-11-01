//! `bb account add|list|remove|default`: stored accounts.
//!
//! Secrets are never command-line arguments (they would end up in the shell
//! history and in the process list): the secret access key comes from
//! standard input (`--secret-stdin`), from `BB_SECRET_ACCESS_KEY`, or from a
//! prompt on the terminal without echo.

use std::io::BufRead;

use bucketbridge_core::model::AccountInput;
use bucketbridge_core::secrets::SecretString;
use bucketbridge_core::{Error, Result, Session};

use crate::cli::{AccountAddArgs, AccountCommand, AccountSelector, GlobalArgs};
use crate::context::{Inputs, dirs, env, find_account, open_session, unlock};
use crate::output::{Failure, Output};
use crate::records::{AccountChangeRecord, AccountRecord};

/// Runs an `account` subcommand.
pub async fn run(
    global: &GlobalArgs,
    command: &AccountCommand,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    if global.account.is_some() || global.mfa_code.is_some() {
        return Err(Error::Usage(
            "--account and --mfa-code do not apply to `bb account`; name the account as an argument"
                .into(),
        )
        .into());
    }
    let dirs = dirs(global, &inputs.env)?;
    let session = open_session(dirs.as_ref(), &inputs.secrets)?;
    match command {
        AccountCommand::Add(args) => add(&session, args, inputs, out).await,
        AccountCommand::List => list(&session, out).await,
        AccountCommand::Remove(selector) => remove(&session, selector, out).await,
        AccountCommand::Default(selector) => set_default(&session, selector, out).await,
    }
}

async fn add(
    session: &Session,
    args: &AccountAddArgs,
    inputs: &Inputs,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    // Storing a secret under a master password needs the key.
    unlock(session, inputs).await?;
    let (secret, session_token) = read_secrets(args, inputs)?;
    let input = AccountInput {
        label: args.label.clone(),
        access_key_id: args.access_key_id.clone(),
        secret_access_key: secret.expose().to_owned(),
        session_token: session_token
            .as_ref()
            .map(|token| token.expose().to_owned()),
        region: args.region.clone(),
        endpoint: args.endpoint.clone(),
        path_style: args.path_style,
        request_payer: args.request_payer,
        mfa_serial: args.mfa_serial.clone(),
    };
    drop(secret);
    drop(session_token);
    let account = session.add_account(input).await?;
    let list = session.list_accounts().await?;
    let record = AccountRecord {
        is_default: list.default_account_id.as_deref() == Some(account.id.as_str()),
        account,
    };
    out.change(&record, || {
        format!(
            "added account {} ({})",
            record.account.label, record.account.id
        )
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
            .map_err(|_| Error::Internal("standard input is unavailable".into()))?;
        let secret = read_line(&mut **stdin)?.ok_or_else(|| {
            Error::Usage("--secret-stdin: standard input is empty; expected the secret access key on the first line".into())
        })?;
        let token = read_line(&mut **stdin)?;
        return Ok((secret, token));
    }
    let secret = match inputs.env.get(env::SECRET_ACCESS_KEY)? {
        Some(secret) => SecretString::new(secret),
        None => match inputs.console.read_secret("Secret access key: ") {
            Ok(Some(secret)) if !secret.is_empty() => SecretString::new(secret),
            _ => {
                return Err(Error::Usage(format!(
                    "no secret access key: use --secret-stdin, set {}, or run bb in a terminal",
                    env::SECRET_ACCESS_KEY
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
                "{} {:<24}  {:<14}  {:<30}  {}",
                if record.is_default { "*" } else { " " },
                record.account.label,
                record.account.region,
                record.account.endpoint.as_deref().unwrap_or("-"),
                record.account.id
            )
        })?;
    }
    Ok(())
}

async fn remove(
    session: &Session,
    selector: &AccountSelector,
    out: &mut Output,
) -> std::result::Result<(), Failure> {
    let record = select(session, selector).await?;
    session.remove_account(&record.id).await?;
    out.change(&record, || {
        format!("removed account {} ({})", record.label, record.id)
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
        format!("default account is now {} ({})", record.label, record.id)
    })
}

async fn select(session: &Session, selector: &AccountSelector) -> Result<AccountChangeRecord> {
    let list = session.list_accounts().await?;
    let account = find_account(&list.accounts, &selector.label_or_id).ok_or_else(|| {
        Error::NotFound(format!(
            "account '{}' not found; `bb account list` shows the stored accounts",
            selector.label_or_id
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
