//! What every command needs: the settings, the account and an S3 transport.

use std::io::BufRead;
use std::sync::{Arc, Mutex};

use bucketbridge_core::cloudfront::{self, CloudFront};
use bucketbridge_core::engine::StopSignal;
use bucketbridge_core::iam::{self, Iam};
use bucketbridge_core::model::{Account, DEFAULT_REGION};
use bucketbridge_core::secrets::{KdfCost, SecretStore, SecretString};
use bucketbridge_core::session::{CloudFrontConfigHook, IamConfigHook, SessionOptions};
use bucketbridge_core::{
    AppConfig, ClientOptions, ConfigStore, Credentials, Dirs, Error, Result, S3, Session, Settings,
};

use crate::cli::GlobalArgs;
use crate::io::{Console, Env};

/// Environment variables read by `bb`.
pub mod env {
    /// Access key ID of the environment account.
    pub const ACCESS_KEY_ID: &str = "BB_ACCESS_KEY_ID";
    /// Secret access key of the environment account (and of `account add`).
    pub const SECRET_ACCESS_KEY: &str = "BB_SECRET_ACCESS_KEY";
    /// Session token of the environment account (and of `account add`).
    pub const SESSION_TOKEN: &str = "BB_SESSION_TOKEN";
    /// Signing region of the environment account.
    pub const REGION: &str = "BB_REGION";
    /// Custom endpoint URL of the environment account.
    pub const ENDPOINT: &str = "BB_ENDPOINT";
    /// Path-style addressing of the environment account.
    pub const PATH_STYLE: &str = "BB_PATH_STYLE";
    /// Directory holding `config.toml`.
    pub const CONFIG_DIR: &str = "BB_CONFIG_DIR";
    /// Master password for stored secrets.
    pub const MASTER_PASSWORD: &str = "BB_MASTER_PASSWORD";
    /// API token of the URL shortener, instead of the stored one.
    pub const SHORTENER_TOKEN: &str = "BB_SHORTENER_TOKEN";
    /// Replaces the endpoint of the chosen URL shortener (a compatible
    /// service of your own, or a local server in tests).
    pub const SHORTENER_ENDPOINT: &str = "BB_SHORTENER_ENDPOINT";
    /// Replaces the address `bb version --check` asks for the latest release
    /// (a mirror of your own, or a local server in tests).
    pub const UPDATE_CHECK_URL: &str = "BB_UPDATE_CHECK_URL";
}

/// Label and ID of the account built from the environment.
pub const ENVIRONMENT_ACCOUNT: &str = "environment";

/// Where stored secrets live.
#[derive(Clone)]
pub enum Secrets {
    /// The OS keychain (what the `bb` binary uses).
    Keychain,
    /// An injected store, with the key-derivation cost of new master
    /// passwords. For running commands in-process (tests).
    Store {
        /// The store.
        store: Arc<dyn SecretStore>,
        /// Key-derivation cost for new master passwords.
        kdf_cost: KdfCost,
        /// Customizes the IAM client (a test injects a mock this way).
        iam_config_hook: Option<IamConfigHook>,
        /// Customizes the CloudFront client (a test injects a scripted
        /// CloudFront this way).
        cloudfront_config_hook: Option<CloudFrontConfigHook>,
        /// Adjusts the session options (a test points the credential
        /// sources at local files and mock services this way).
        options_hook: Option<SessionOptionsHook>,
    },
}

/// Adjusts the [`SessionOptions`] of an in-process session.
pub type SessionOptionsHook = Arc<dyn Fn(SessionOptions) -> SessionOptions + Send + Sync>;

impl Secrets {
    fn cloudfront_hook(&self) -> Option<CloudFrontConfigHook> {
        match self {
            Secrets::Store {
                cloudfront_config_hook,
                ..
            } => cloudfront_config_hook.clone(),
            Secrets::Keychain => None,
        }
    }
}

/// Where the CloudFront handle of the selected account comes from.
enum CloudFrontSource {
    /// The `BB_*` account.
    Environment {
        account: Box<Account>,
        credentials: Credentials,
        options: Box<ClientOptions>,
        hook: Option<CloudFrontConfigHook>,
    },
    /// A stored account, through its session.
    Stored {
        session: Box<Session>,
        account_id: String,
    },
    /// A profile of the AWS files used without being stored.
    Profile {
        session: Box<Session>,
        profile: String,
        options: bucketbridge_core::session::ProfileOptions,
    },
}

/// The inputs of an invocation besides its arguments.
pub struct Inputs {
    /// Environment variables.
    pub env: Env,
    /// Standard input.
    pub stdin: Mutex<Box<dyn BufRead + Send>>,
    /// Terminal prompts.
    pub console: Arc<dyn Console>,
    /// Where stored secrets live.
    pub secrets: Secrets,
}

/// What `bb presign --shorten` needs besides the settings.
#[derive(Clone)]
pub struct ShortenerAccess {
    /// `BB_SHORTENER_TOKEN`, when set.
    pub token: Option<SecretString>,
    /// `BB_SHORTENER_ENDPOINT`, when set.
    pub endpoint: Option<String>,
    /// Where the stored shortener token lives.
    pub secrets: Secrets,
}

/// Settings, account and transport of one invocation.
pub struct Context {
    /// Transport of the selected account.
    pub s3: S3,
    /// ID of the selected account (`environment` for the account built from
    /// the `BB_*` variables); plans of `bb sync` are stored with it.
    pub account_id: String,
    /// The directories of this invocation (`None` when there is no home
    /// directory and none was given): `data_dir` holds the sync plans.
    pub dirs: Option<Dirs>,
    /// Settings from `config.toml`.
    pub settings: Settings,
    /// Stops the transfers of this invocation (Ctrl-C cancels it).
    pub stop: StopSignal,
    /// Whether transfers draw a progress line: only on a terminal, and
    /// neither with `--quiet` nor with `--json`.
    pub show_progress: bool,
    /// Whether questions can be asked on the terminal.
    pub can_ask: bool,
    /// The terminal, for confirmations.
    pub console: Arc<dyn Console>,
    cloudfront: CloudFrontSource,
    /// Environment and secret store for the URL shortener.
    pub shortener: ShortenerAccess,
}

/// The transport of the account named by `selector` (a label, an ID or
/// `environment`), for commands that touch two accounts (`bb cp --from-account`).
/// Stored secrets are unlocked like for the main account.
pub async fn account_s3(selector: &str, global: &GlobalArgs, inputs: &Inputs) -> Result<S3> {
    let dirs = dirs(global, &inputs.env)?;
    let config = load_config(dirs.as_ref())?;
    match resolve_account(Some(selector), &config)? {
        Selection::Environment => {
            let (account, credentials) = environment_account(&inputs.env)?;
            S3::new(
                &account,
                &credentials,
                &ClientOptions::from_settings(&config.settings),
            )
        }
        Selection::Stored(account) => {
            let session = open_session(dirs.as_ref(), &inputs.secrets)?;
            unlock(&session, inputs).await?;
            session.s3_for(&account.id).await
        }
    }
}

/// The directories of this invocation: `--config-dir`, else `BB_CONFIG_DIR`,
/// else the platform directories (`None` when there is no home directory).
pub fn dirs(global: &GlobalArgs, env: &Env) -> Result<Option<Dirs>> {
    if let Some(dir) = &global.config_dir {
        return Dirs::from_override(Some(dir.as_os_str().to_owned())).map(Some);
    }
    let value = env.get_os(env::CONFIG_DIR).cloned();
    match Dirs::from_override(value.clone()) {
        Ok(dirs) => Ok(Some(dirs)),
        // No home directory: only the environment account can work.
        Err(_) if value.as_ref().is_none_or(|v| v.is_empty()) => Ok(None),
        Err(err) => Err(err),
    }
}

/// `config.toml`: defaults when there is no directory or no file; an
/// existing file that cannot be used is an error.
pub fn load_config(dirs: Option<&Dirs>) -> Result<AppConfig> {
    match dirs {
        Some(dirs) => ConfigStore::new(dirs).load(),
        None => Ok(AppConfig::default()),
    }
}

/// Opens the session that manages stored accounts and their secrets.
pub fn open_session(dirs: Option<&Dirs>, secrets: &Secrets) -> Result<Session> {
    let dirs = dirs.cloned().ok_or_else(|| {
        Error::Usage(crate::i18n::trf(
            "no configuration directory: set {env} or pass --config-dir",
            &[("env", env::CONFIG_DIR)],
        ))
    })?;
    match secrets {
        Secrets::Keychain => Session::open(dirs),
        Secrets::Store {
            store,
            kdf_cost,
            iam_config_hook,
            cloudfront_config_hook,
            options_hook,
        } => {
            let options = SessionOptions {
                kdf_cost: *kdf_cost,
                cloudfront_config_hook: cloudfront_config_hook.clone(),
                iam_config_hook: iam_config_hook.clone(),
                ..SessionOptions::default()
            };
            let options = match options_hook {
                Some(hook) => hook(options),
                None => options,
            };
            Session::open_with(dirs, Arc::clone(store), options)
        }
    }
}

/// A stored account named by ID (exactly) or label (case-insensitively).
pub fn find_account<'a>(accounts: &'a [Account], selector: &str) -> Option<&'a Account> {
    accounts
        .iter()
        .find(|account| account.id == selector)
        .or_else(|| {
            accounts
                .iter()
                .find(|account| account.label.eq_ignore_ascii_case(selector))
        })
}

/// Which account a command runs as.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)] // one value per invocation
pub enum Selection {
    /// A stored account.
    Stored(Account),
    /// The account built from the `BB_*` variables.
    Environment,
}

/// Resolves the account: `--account` (a stored account by ID or label, or
/// `environment`), else the default account, else the environment account.
/// This is the only place that decides it.
pub fn resolve_account(selector: Option<&str>, config: &AppConfig) -> Result<Selection> {
    if let Some(selector) = selector {
        if let Some(account) = find_account(&config.accounts, selector) {
            return Ok(Selection::Stored(account.clone()));
        }
        if selector.eq_ignore_ascii_case(ENVIRONMENT_ACCOUNT) {
            return Ok(Selection::Environment);
        }
        return Err(Error::NotFound(crate::i18n::trf(
            "account '{selector}' not found; `bb account list` shows the stored accounts",
            &[("selector", selector)],
        )));
    }
    let default = config
        .default_account_id
        .as_deref()
        .and_then(|id| config.accounts.iter().find(|account| account.id == id));
    Ok(match default {
        Some(account) => Selection::Stored(account.clone()),
        None => Selection::Environment,
    })
}

/// The profile named by `--profile` for a command that runs as an account:
/// at most one, and not together with `--account` or `--mfa-code`.
fn profile_selector(global: &GlobalArgs) -> Result<Option<String>> {
    match global.profile.as_slice() {
        [] => Ok(None),
        [profile] => {
            if global.account.is_some() || global.mfa_code.is_some() {
                return Err(Error::Usage(crate::i18n::tr(
                    "--profile cannot be combined with --account or --mfa-code",
                )));
            }
            Ok(Some(profile.clone()))
        }
        _ => Err(Error::Usage(crate::i18n::tr(
            "--profile can be given once for a command (several only to `bb account import-aws`)",
        ))),
    }
}

/// What `--profile` takes from the environment besides the profile:
/// `BB_REGION`, `BB_ENDPOINT` and `BB_PATH_STYLE`, as for the environment
/// account.
fn profile_options(env: &Env) -> Result<bucketbridge_core::session::ProfileOptions> {
    let path_style = match env.get(env::PATH_STYLE)? {
        Some(value) => parse_bool(env::PATH_STYLE, &value)?,
        None => false,
    };
    Ok(bucketbridge_core::session::ProfileOptions {
        region: env.get(env::REGION)?,
        endpoint: env.get(env::ENDPOINT)?,
        path_style,
    })
}

/// Unlocks stored secrets when a master password is set: the password comes
/// from `BB_MASTER_PASSWORD` or the terminal; without either the secrets
/// stay `locked`. A wrong password is an `auth` error.
pub async fn unlock(session: &Session, inputs: &Inputs) -> Result<()> {
    let status = session.security_status().await?;
    if !status.master_password_set || status.unlocked {
        return Ok(());
    }
    let password = match inputs.env.get(env::MASTER_PASSWORD)? {
        Some(password) => SecretString::new(password),
        None => match inputs
            .console
            .read_secret(&crate::i18n::tr("Master password: "))
        {
            Ok(Some(password)) => SecretString::new(password),
            _ => return Err(Error::Locked),
        },
    };
    session.unlock(password.expose()).await?;
    Ok(())
}

impl Context {
    /// Loads the settings, resolves the account and builds its transport,
    /// unlocking stored secrets and logging in with `--mfa-code` as needed.
    pub async fn new(global: &GlobalArgs, inputs: &Inputs) -> Result<Self> {
        let dirs = dirs(global, &inputs.env)?;
        let config = load_config(dirs.as_ref())?;
        let settings = config.settings.clone();
        let profile = profile_selector(global)?;
        let (s3, account_id, cloudfront) = if let Some(profile) = profile {
            let session = open_session(dirs.as_ref(), &inputs.secrets)?;
            let s3 = session
                .s3_for_profile(&profile, &profile_options(&inputs.env)?)
                .await?;
            let account_id = format!("profile:{profile}");
            let source = CloudFrontSource::Profile {
                session: Box::new(session),
                profile,
                options: profile_options(&inputs.env)?,
            };
            (s3, account_id, source)
        } else {
            match resolve_account(global.account.as_deref(), &config)? {
                Selection::Environment => {
                    if global.mfa_code.is_some() {
                        return Err(Error::Usage(crate::i18n::tr(
                            "--mfa-code needs a stored account with an MFA device",
                        )));
                    }
                    let (account, credentials) = environment_account(&inputs.env)?;
                    let options = ClientOptions::from_settings(&settings);
                    let s3 = S3::new(&account, &credentials, &options)?;
                    let account_id = account.id.clone();
                    let source = CloudFrontSource::Environment {
                        account: Box::new(account),
                        credentials,
                        options: Box::new(options),
                        hook: inputs.secrets.cloudfront_hook(),
                    };
                    (s3, account_id, source)
                }
                Selection::Stored(account) => {
                    let session = open_session(dirs.as_ref(), &inputs.secrets)?;
                    unlock(&session, inputs).await?;
                    if let Some(code) = &global.mfa_code {
                        session.mfa_login(&account.id, code).await?;
                    }
                    let s3 = session.s3_for(&account.id).await?;
                    let account_id = account.id;
                    let source = CloudFrontSource::Stored {
                        session: Box::new(session),
                        account_id: account_id.clone(),
                    };
                    (s3, account_id, source)
                }
            }
        };
        let shortener = ShortenerAccess {
            token: inputs.env.get(env::SHORTENER_TOKEN)?.map(SecretString::new),
            endpoint: inputs.env.get(env::SHORTENER_ENDPOINT)?,
            secrets: inputs.secrets.clone(),
        };
        Ok(Context {
            s3,
            account_id,
            dirs,
            settings,
            stop: StopSignal::new(),
            show_progress: inputs.console.stderr_is_terminal() && !global.quiet && !global.json,
            can_ask: inputs.console.can_ask(),
            console: Arc::clone(&inputs.console),
            cloudfront,
            shortener,
        })
    }

    /// The CloudFront handle of the selected account. Fails with `usage`
    /// for an account with a custom endpoint, which has no CloudFront.
    pub async fn cloudfront(&self) -> Result<CloudFront> {
        match &self.cloudfront {
            CloudFrontSource::Stored {
                session,
                account_id,
            } => session.cloudfront_for(account_id).await,
            CloudFrontSource::Profile {
                session,
                profile,
                options,
            } => session.cloudfront_for_profile(profile, options).await,
            CloudFrontSource::Environment {
                account,
                credentials,
                options,
                hook,
            } => {
                let mut builder = cloudfront::cloudfront_config(account, credentials, options)?;
                if let Some(hook) = hook {
                    builder = hook(builder);
                }
                Ok(CloudFront::from_client(
                    bucketbridge_core::aws_sdk_cloudfront::Client::from_conf(builder.build()),
                ))
            }
        }
    }
}

/// The IAM client of the selected account (see [`Context::new`] for how the
/// account is chosen). Fails with `usage` for an account with a custom
/// endpoint.
pub async fn iam_client(global: &GlobalArgs, inputs: &Inputs) -> Result<Iam> {
    let dirs = dirs(global, &inputs.env)?;
    let config = load_config(dirs.as_ref())?;
    if let Some(profile) = profile_selector(global)? {
        let session = open_session(dirs.as_ref(), &inputs.secrets)?;
        return session
            .iam_for_profile(&profile, &profile_options(&inputs.env)?)
            .await;
    }
    match resolve_account(global.account.as_deref(), &config)? {
        Selection::Environment => {
            if global.mfa_code.is_some() {
                return Err(Error::Usage(crate::i18n::tr(
                    "--mfa-code needs a stored account with an MFA device",
                )));
            }
            let (account, credentials) = environment_account(&inputs.env)?;
            let options = ClientOptions::from_settings(&config.settings);
            let mut builder = iam::iam_config(&account, &credentials, &options)?;
            if let Secrets::Store {
                iam_config_hook: Some(hook),
                ..
            } = &inputs.secrets
            {
                builder = hook(builder);
            }
            Ok(Iam::from_config(builder.build(), options.read_timeout))
        }
        Selection::Stored(account) => {
            let session = open_session(dirs.as_ref(), &inputs.secrets)?;
            unlock(&session, inputs).await?;
            if let Some(code) = &global.mfa_code {
                session.mfa_login(&account.id, code).await?;
            }
            session.iam_for(&account.id).await
        }
    }
}

/// Parses a boolean environment value.
pub fn parse_bool(name: &str, value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(Error::Usage(crate::i18n::trf(
            "{name} must be true or false (also 1/0, yes/no, on/off), got {value}",
            &[("name", name), ("value", value)],
        ))),
    }
}

/// The ephemeral account described by the `BB_*` environment variables.
fn environment_account(env: &Env) -> Result<(Account, Credentials)> {
    let access_key = env.get(env::ACCESS_KEY_ID)?;
    let secret_key = env.get(env::SECRET_ACCESS_KEY)?;
    let (Some(access_key), Some(secret_key)) = (access_key, secret_key) else {
        return Err(Error::Auth(crate::i18n::trf(
            "no credentials: set {key_env} and {secret_env}, or add an account with `bb account add`",
            &[
                ("key_env", env::ACCESS_KEY_ID),
                ("secret_env", env::SECRET_ACCESS_KEY),
            ],
        )));
    };
    let path_style = match env.get(env::PATH_STYLE)? {
        Some(value) => parse_bool(env::PATH_STYLE, &value)?,
        None => false,
    };
    let account = Account {
        source: bucketbridge_core::model::CredentialSource::StaticKeys,
        id: ENVIRONMENT_ACCOUNT.to_owned(),
        label: ENVIRONMENT_ACCOUNT.to_owned(),
        access_key_id: access_key.clone(),
        secret_ref: String::new(),
        session_token_ref: None,
        region: env
            .get(env::REGION)?
            .unwrap_or_else(|| DEFAULT_REGION.to_owned()),
        endpoint: env.get(env::ENDPOINT)?,
        path_style,
        request_payer: false,
        mfa_serial: None,
        options: Default::default(),
    };
    let credentials = Credentials::new(access_key, secret_key, env.get(env::SESSION_TOKEN)?);
    Ok((account, credentials))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bucketbridge_core::ErrorKind;

    fn account(id: &str, label: &str) -> Account {
        Account {
            id: id.into(),
            label: label.into(),
            ..Account::default()
        }
    }

    #[test]
    fn booleans() {
        for yes in ["1", "true", "TRUE", "yes", "on", " On "] {
            assert!(parse_bool("X", yes).unwrap(), "{yes}");
        }
        for no in ["0", "false", "No", "off"] {
            assert!(!parse_bool("X", no).unwrap(), "{no}");
        }
        assert_eq!(
            parse_bool("BB_PATH_STYLE", "maybe").unwrap_err().kind(),
            ErrorKind::Usage
        );
    }

    #[test]
    fn account_resolution_order() {
        let mut config = AppConfig::default();
        assert_eq!(
            resolve_account(None, &config).unwrap(),
            Selection::Environment
        );
        config.accounts = vec![account("id-1", "Work"), account("id-2", "Home")];
        assert_eq!(
            resolve_account(None, &config).unwrap(),
            Selection::Environment
        );
        config.default_account_id = Some("id-2".into());
        assert_eq!(
            resolve_account(None, &config).unwrap(),
            Selection::Stored(account("id-2", "Home"))
        );
        assert_eq!(
            resolve_account(Some("work"), &config).unwrap(),
            Selection::Stored(account("id-1", "Work"))
        );
        assert_eq!(
            resolve_account(Some("id-1"), &config).unwrap(),
            Selection::Stored(account("id-1", "Work"))
        );
        assert_eq!(
            resolve_account(Some("ENVIRONMENT"), &config).unwrap(),
            Selection::Environment
        );
        let err = resolve_account(Some("nope"), &config).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);
        // A dangling default falls back to the environment account.
        config.default_account_id = Some("gone".into());
        assert_eq!(
            resolve_account(None, &config).unwrap(),
            Selection::Environment
        );
        // A stored account labelled "environment" wins over the keyword.
        config.accounts.push(account("id-3", "Environment"));
        assert_eq!(
            resolve_account(Some("environment"), &config).unwrap(),
            Selection::Stored(account("id-3", "Environment"))
        );
    }

    #[test]
    fn ids_win_over_labels() {
        let accounts = [account("a", "b"), account("b", "c")];
        assert_eq!(
            find_account(&accounts, "b").map(|a| a.id.as_str()),
            Some("b")
        );
        assert_eq!(
            find_account(&accounts, "C").map(|a| a.id.as_str()),
            Some("b")
        );
        assert!(find_account(&accounts, "x").is_none());
    }

    #[test]
    fn environment_account_needs_both_keys() {
        let err = environment_account(&Env::from_pairs([(env::ACCESS_KEY_ID, "AK")])).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Auth);
        let (account, credentials) = environment_account(&Env::from_pairs([
            (env::ACCESS_KEY_ID, "AK"),
            (env::SECRET_ACCESS_KEY, "SK"),
            (env::REGION, "eu-west-1"),
            (env::PATH_STYLE, "yes"),
        ]))
        .unwrap();
        assert_eq!(account.region, "eu-west-1");
        assert!(account.path_style);
        assert_eq!(credentials.access_key_id, "AK");
    }

    #[test]
    fn explicit_config_dir_is_used() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[settings.advanced]\nlist_page_size = 7\n",
        )
        .unwrap();
        let global = GlobalArgs {
            account: None,
            profile: Vec::new(),
            mfa_code: None,
            json: false,
            quiet: false,
            config_dir: Some(dir.path().to_owned()),
        };
        let dirs = dirs(&global, &Env::default()).unwrap();
        assert_eq!(
            load_config(dirs.as_ref())
                .unwrap()
                .settings
                .advanced
                .list_page_size,
            7
        );
        std::fs::write(dir.path().join("config.toml"), "= =").unwrap();
        assert_eq!(
            load_config(dirs.as_ref()).unwrap_err().kind(),
            ErrorKind::Io
        );
    }
}
