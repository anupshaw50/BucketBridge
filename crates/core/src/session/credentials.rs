//! The parts of [`Session`] that deal with credential sources other than
//! static keys (see [`crate::creds`]): building credentials providers,
//! importing AWS profiles, probing the instance role and the IAM Identity
//! Center sign-in.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials as SdkCredentials;
use aws_credential_types::provider::SharedCredentialsProvider;
use aws_smithy_async::time::SharedTimeSource;
use aws_smithy_runtime_api::client::http::SharedHttpClient;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::{Session, account_in};
use crate::client::Credentials;
use crate::config::{AppConfig, new_uuid_v4};
use crate::creds::AwsFiles;
use crate::creds::assume::{AssumeRoleCredentials, RoleSpec};
use crate::creds::instance::{self, InstanceRoleProbe};
use crate::creds::profiles::{self, AwsProfileEntry, ImportOutcome, SkippedProfile, free_label};
use crate::creds::sso::{
    self, DeviceLogin, Poll, Portal, SignInStatus, SsoAccount, SsoCredentials,
};
use crate::error::{Error, Result};
use crate::mfa;
use crate::model::{Account, AccountInput, CredentialSource};
use crate::ops::S3;

/// The longest chain of assumed roles BucketBridge follows.
const MAX_ROLE_CHAIN: usize = 5;

/// Sign-ins that wait for the user, at most this many at a time.
const MAX_PENDING_LOGINS: usize = 4;

/// A pending sign-in is forgotten this long after it was started, whatever
/// the service says.
const PENDING_LOGIN_LIFETIME: Duration = Duration::from_secs(20 * 60);

type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SharedCredentialsProvider>> + Send + 'a>>;

/// How an unstored profile (`bb --profile NAME`) reaches S3: what the
/// profile does not say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileOptions {
    /// Signing region; default the profile's, `AWS_REGION`,
    /// `AWS_DEFAULT_REGION`, then `us-east-1`.
    pub region: Option<String>,
    /// Custom endpoint of an S3-compatible provider.
    pub endpoint: Option<String>,
    /// Path-style addressing.
    pub path_style: bool,
}

/// The sign-ins in progress, by login id.
#[derive(Default)]
pub(super) struct SsoLogins {
    pending: HashMap<String, (Instant, DeviceLogin)>,
}

/// What the user needs to complete an IAM Identity Center sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsoLoginStart {
    /// Handle to poll the sign-in with.
    pub login_id: String,
    /// The code to enter at the verification page.
    pub user_code: String,
    /// The page to open.
    pub verification_uri: String,
    /// The page with the code filled in, when offered.
    pub verification_uri_complete: Option<String>,
    /// Seconds until the code expires.
    pub expires_in_secs: u64,
    /// Seconds to wait between polls.
    pub interval_secs: u64,
}

/// Result of polling a sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SsoLoginPoll {
    /// Waiting for the user to approve in the browser.
    Pending {
        /// Seconds to wait before the next poll.
        interval_secs: u64,
    },
    /// Signed in; the token is in the AWS CLI cache.
    Complete {
        /// When the access token expires (RFC 3339).
        expires_at: String,
    },
}

fn fixed(credentials: &Credentials) -> SharedCredentialsProvider {
    SharedCredentialsProvider::new(SdkCredentials::new(
        credentials.access_key_id.as_str(),
        credentials.secret_access_key.as_str(),
        credentials.session_token.clone(),
        None,
        "BucketBridge",
    ))
}

impl Session {
    /// Where the AWS shared files and the SSO token cache are.
    pub fn aws_files(&self) -> AwsFiles {
        self.creds_hooks.files()
    }

    /// The accounts an assumed role signs through, with the MFA session of
    /// each: what a cached S3 handle of the role was built from.
    pub(super) fn source_chain(
        &self,
        config: &AppConfig,
        account: &Account,
    ) -> Vec<(Account, Option<SystemTime>)> {
        let mut chain: Vec<(Account, Option<SystemTime>)> = Vec::new();
        let mut next = account;
        while let Some((source_id, _)) = RoleSpec::from_source(&next.source) {
            let Some(source) = config.accounts.iter().find(|a| a.id == source_id) else {
                break;
            };
            if chain.len() >= MAX_ROLE_CHAIN || chain.iter().any(|(a, _)| a.id == source.id) {
                break;
            }
            chain.push((source.clone(), self.mfa.usable_until_for(&source.id)));
            next = source;
        }
        chain
    }

    /// The credentials provider of an account of any kind. Static keys with
    /// an MFA device and roles with one give the credentials of their MFA
    /// session (`mfa_required` without one); an assumed role without a
    /// device gives a provider that assumes it again before it expires.
    pub(super) fn provider_for<'a>(
        &'a self,
        config: &'a AppConfig,
        account: &'a Account,
        http_client: &'a SharedHttpClient,
        visiting: &'a mut Vec<String>,
    ) -> ProviderFuture<'a> {
        Box::pin(async move {
            if visiting.contains(&account.id) || visiting.len() >= MAX_ROLE_CHAIN {
                return Err(Error::Usage(
                    "the accounts a role is assumed through form a loop or a chain that is too long"
                        .into(),
                ));
            }
            visiting.push(account.id.clone());
            match &account.source {
                CredentialSource::StaticKeys => {
                    if account.needs_mfa_session() {
                        Ok(fixed(&self.mfa.active(account)?.0))
                    } else {
                        let manager = Arc::clone(&self.manager);
                        let id = account.id.clone();
                        let (_, credentials) = self
                            .managed(move || manager.account_credentials(&id))
                            .await?;
                        Ok(fixed(&credentials))
                    }
                }
                CredentialSource::AwsProfile { profile } => Ok(profiles::profile_provider(
                    profile,
                    &self.aws_files(),
                    http_client.clone(),
                    &account.region,
                )),
                CredentialSource::InstanceRole => {
                    let (_, provider) = instance::instance_provider(
                        http_client.clone(),
                        self.creds_hooks.imds_endpoint.as_deref(),
                    )?;
                    Ok(provider)
                }
                CredentialSource::Sso {
                    start_url,
                    sso_region,
                    account_id,
                    role_name,
                    sso_session,
                } => {
                    let portal = Portal::new(start_url, sso_region, sso_session.as_deref())?;
                    let oidc = sso::oidc_client(
                        sso_region,
                        http_client.clone(),
                        self.creds_hooks.sso_oidc.as_ref(),
                    );
                    let portal_api = sso::portal_client(
                        sso_region,
                        http_client.clone(),
                        self.creds_hooks.sso.as_ref(),
                    );
                    Ok(SharedCredentialsProvider::new(SsoCredentials::new(
                        portal,
                        oidc,
                        portal_api,
                        self.aws_files().sso_cache().to_path_buf(),
                        account_id,
                        role_name,
                        SharedTimeSource::default(),
                    )))
                }
                CredentialSource::AssumeRole { .. } => {
                    if account.needs_mfa_session() {
                        return Ok(fixed(&self.mfa.active(account)?.0));
                    }
                    let (client, spec) = self
                        .role_client(config, account, http_client, visiting, true)
                        .await?;
                    Ok(SharedCredentialsProvider::new(AssumeRoleCredentials::new(
                        client,
                        spec,
                        SharedTimeSource::default(),
                    )))
                }
            }
        })
    }

    /// The STS client of an assumed role, signed with the credentials of
    /// its source account. `retry` turns the SDK's retries on (not for an
    /// MFA login, where a code must be used once).
    async fn role_client(
        &self,
        config: &AppConfig,
        account: &Account,
        http_client: &SharedHttpClient,
        visiting: &mut Vec<String>,
        retry: bool,
    ) -> Result<(aws_sdk_sts::Client, RoleSpec)> {
        let (source_id, spec) = RoleSpec::from_source(&account.source)
            .ok_or_else(|| Error::Usage("the account does not assume a role".into()))?;
        let source = account_in(config, &source_id)?;
        let source_provider = self
            .provider_for(config, &source, http_client, visiting)
            .await?;
        let mut builder = mfa::sts_config_with_provider(account, source_provider)?
            .http_client(http_client.clone());
        if retry {
            builder = builder.retry_config(
                aws_sdk_sts::config::retry::RetryConfig::standard().with_max_attempts(3),
            );
        }
        if let Some(hook) = &self.sts_config_hook {
            builder = hook(builder);
        }
        Ok((aws_sdk_sts::Client::from_conf(builder.build()), spec))
    }

    /// The STS client of [`Session::mfa_login`]: for static keys signed
    /// with the account's keys (`GetSessionToken`), for an assumed role
    /// with an MFA device signed through its source account
    /// (`AssumeRole`). The flag is `true` for a role.
    pub(super) async fn mfa_login_client(
        &self,
        account_id: &str,
        http_client: SharedHttpClient,
    ) -> Result<(Account, aws_sdk_sts::Client, bool)> {
        let manager = Arc::clone(&self.manager);
        let config = self.managed(move || manager.load()).await?;
        let stored = account_in(&config, account_id)?;
        if stored.has_static_keys() {
            let manager = Arc::clone(&self.manager);
            let owned_id = account_id.to_owned();
            let (account, credentials) = self
                .managed(move || manager.account_credentials(&owned_id))
                .await?;
            let mut builder = mfa::sts_config(&account, &credentials)?.http_client(http_client);
            drop(credentials);
            if let Some(hook) = &self.sts_config_hook {
                builder = hook(builder);
            }
            return Ok((
                account,
                aws_sdk_sts::Client::from_conf(builder.build()),
                false,
            ));
        }
        if stored.source.role_mfa_serial().is_none() {
            return Err(Error::Usage(
                "the account has no MFA device to log in with".into(),
            ));
        }
        let mut visiting = Vec::new();
        let (client, _) = self
            .role_client(&config, &stored, &http_client, &mut visiting, false)
            .await?;
        Ok((stored, client, true))
    }

    /// The account a profile of the AWS files stands for when it is used
    /// without being stored (`bb --profile NAME`): the region is the
    /// profile's, else `AWS_REGION` or `AWS_DEFAULT_REGION`, else
    /// `us-east-1`. Fails with `not_found` when the profile does not exist.
    pub async fn profile_account(
        &self,
        profile: &str,
        options: &ProfileOptions,
    ) -> Result<Account> {
        let files = self.aws_files();
        let found = profiles::find_profile(&files, profile).await?;
        let Some(found) = found else {
            return Err(Error::NotFound(format!(
                "the AWS profile {profile:?} does not exist in {} or {}",
                files.config().display(),
                files.credentials().display()
            )));
        };
        let region = options
            .region
            .clone()
            .or(found.region)
            .or_else(|| std::env::var("AWS_REGION").ok())
            .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
            .filter(|region| !region.is_empty())
            .unwrap_or_else(|| crate::model::DEFAULT_REGION.to_owned());
        Ok(Account {
            id: format!("profile:{profile}"),
            label: profile.to_owned(),
            region,
            endpoint: options.endpoint.clone(),
            path_style: options.path_style,
            source: CredentialSource::AwsProfile {
                profile: profile.to_owned(),
            },
            ..Account::default()
        })
    }

    /// The configuration and provider of an unstored profile account.
    async fn profile_signing(
        &self,
        profile: &str,
        options: &ProfileOptions,
    ) -> Result<(
        Account,
        AppConfig,
        SharedHttpClient,
        SharedCredentialsProvider,
    )> {
        let account = self.profile_account(profile, options).await?;
        let manager = Arc::clone(&self.manager);
        let config = self.managed(move || manager.load()).await?;
        let http_client = self.http_client().await?;
        let provider = self
            .provider_for(&config, &account, &http_client, &mut Vec::new())
            .await?;
        Ok((account, config, http_client, provider))
    }

    /// An S3 handle for a profile of the AWS files that is not stored as an
    /// account (`bb --profile NAME`). Nothing is cached or saved.
    pub async fn s3_for_profile(
        &self,
        profile: &str,
        profile_options: &ProfileOptions,
    ) -> Result<S3> {
        let (account, config, http_client, provider) =
            self.profile_signing(profile, profile_options).await?;
        let mut options = crate::client::ClientOptions::from_settings(&config.settings);
        options.http_client = Some(http_client);
        S3::with_provider(&account, provider, &options)
    }

    /// The IAM handle of an unstored profile (see [`Session::s3_for_profile`]).
    pub async fn iam_for_profile(
        &self,
        profile: &str,
        profile_options: &ProfileOptions,
    ) -> Result<crate::iam::Iam> {
        let (account, config, http_client, provider) =
            self.profile_signing(profile, profile_options).await?;
        let options = crate::client::ClientOptions::from_settings(&config.settings);
        let mut builder = crate::iam::iam_config_with_provider(&account, provider, &options)?
            .http_client(http_client);
        if let Some(hook) = &self.iam_config_hook {
            builder = hook(builder);
        }
        Ok(crate::iam::Iam::from_config(
            builder.build(),
            options.read_timeout,
        ))
    }

    /// The CloudFront handle of an unstored profile (see
    /// [`Session::s3_for_profile`]).
    pub async fn cloudfront_for_profile(
        &self,
        profile: &str,
        profile_options: &ProfileOptions,
    ) -> Result<crate::cloudfront::CloudFront> {
        let (account, config, http_client, provider) =
            self.profile_signing(profile, profile_options).await?;
        let mut options = crate::client::ClientOptions::from_settings(&config.settings);
        options.http_client = Some(http_client.clone());
        let mut builder =
            crate::cloudfront::cloudfront_config_with_provider(&account, provider, &options)?
                .http_client(http_client);
        if let Some(hook) = &self.cloudfront_config_hook {
            builder = hook(builder);
        }
        Ok(crate::cloudfront::CloudFront::from_client(
            aws_sdk_cloudfront::Client::from_conf(builder.build()),
        ))
    }

    /// The profiles of the AWS files, each with the account that links it.
    pub async fn list_aws_profiles(&self) -> Result<Vec<AwsProfileEntry>> {
        let listed = profiles::list_profiles(&self.aws_files()).await?;
        let accounts = self.list_accounts().await?.accounts;
        Ok(listed
            .into_iter()
            .map(|profile| {
                let account_id = accounts
                    .iter()
                    .find(|account| {
                        matches!(&account.source, CredentialSource::AwsProfile { profile: name }
                            if *name == profile.name)
                    })
                    .map(|account| account.id.clone());
                AwsProfileEntry {
                    profile,
                    account_id,
                }
            })
            .collect())
    }

    /// Imports profiles of the AWS files as accounts that stay linked to
    /// the files: only the profile name is stored. Profiles that are linked
    /// already, are not usable or do not exist are reported in `skipped`.
    pub async fn import_aws_profiles(&self, names: &[String]) -> Result<ImportOutcome> {
        let entries = self.list_aws_profiles().await?;
        let mut taken: Vec<String> = self
            .list_accounts()
            .await?
            .accounts
            .into_iter()
            .map(|account| account.label)
            .collect();
        let mut outcome = ImportOutcome {
            imported: Vec::new(),
            skipped: Vec::new(),
        };
        let mut seen: Vec<&str> = Vec::new();
        for name in names {
            if seen.contains(&name.as_str()) {
                continue;
            }
            seen.push(name);
            let skip = |reason: &str| SkippedProfile {
                profile: name.clone(),
                reason: reason.to_owned(),
            };
            let Some(entry) = entries.iter().find(|entry| entry.profile.name == *name) else {
                outcome.skipped.push(skip("the profile does not exist"));
                continue;
            };
            if entry.account_id.is_some() {
                outcome
                    .skipped
                    .push(skip("the profile is imported already"));
                continue;
            }
            if !entry.profile.importable {
                let reason = entry
                    .profile
                    .note
                    .clone()
                    .unwrap_or_else(|| "the profile cannot be used".to_owned());
                outcome.skipped.push(skip(&reason));
                continue;
            }
            let label = free_label(&taken, name);
            let mut input = AccountInput::default();
            input.label.clone_from(&label);
            input.region = entry
                .profile
                .region
                .clone()
                .unwrap_or_else(|| crate::model::DEFAULT_REGION.to_owned());
            input.source = CredentialSource::AwsProfile {
                profile: name.clone(),
            };
            match self.add_account(input).await {
                Ok(account) => {
                    taken.push(label);
                    outcome.imported.push(account);
                }
                Err(error) => outcome.skipped.push(skip(&error.to_string())),
            }
        }
        Ok(outcome)
    }

    /// Checks that this machine has an instance or container role (see
    /// [`instance::probe`]).
    pub async fn probe_instance_role(&self) -> Result<InstanceRoleProbe> {
        let http_client = self.http_client().await?;
        instance::probe(http_client, self.creds_hooks.imds_endpoint.as_deref()).await
    }

    /// Starts an IAM Identity Center sign-in: registers the OIDC client and
    /// asks for a device code. Show `user_code`, open the verification page
    /// and poll with [`Session::sso_poll_login`].
    pub async fn sso_start_login(
        &self,
        start_url: &str,
        sso_region: &str,
        sso_session: Option<&str>,
    ) -> Result<SsoLoginStart> {
        let portal = Portal::new(start_url, sso_region, sso_session)?;
        let http_client = self.http_client().await?;
        let oidc = sso::oidc_client(
            &portal.region,
            http_client,
            self.creds_hooks.sso_oidc.as_ref(),
        );
        let now = Utc::now();
        let login = sso::login::start_login(&oidc, &portal, now).await?;
        let login_id = new_uuid_v4()?;
        let started = SsoLoginStart {
            login_id: login_id.clone(),
            user_code: login.user_code.clone(),
            verification_uri: login.verification_uri.clone(),
            verification_uri_complete: login.verification_uri_complete.clone(),
            expires_in_secs: (login.expires_at - now).num_seconds().max(0).unsigned_abs(),
            interval_secs: login.interval.as_secs(),
        };
        let mut logins = self
            .sso_logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        logins
            .pending
            .retain(|_, (started, _)| now.duration_since(*started) < PENDING_LOGIN_LIFETIME);
        while logins.pending.len() >= MAX_PENDING_LOGINS {
            let oldest = logins
                .pending
                .iter()
                .min_by_key(|(_, (started, _))| *started)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    logins.pending.remove(&id);
                }
                None => break,
            }
        }
        logins.pending.insert(login_id, (now, login));
        Ok(started)
    }

    /// Polls a sign-in once. When the user approved, the token is stored in
    /// the AWS CLI cache (mode 0600) and the sign-in ends; it also ends on
    /// `auth` failures (denied, expired). Other failures keep it, so the
    /// caller may poll again.
    pub async fn sso_poll_login(&self, login_id: &str) -> Result<SsoLoginPoll> {
        let taken = self
            .sso_logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .remove(login_id);
        let Some((started, mut login)) = taken else {
            return Err(Error::NotFound(
                "there is no such sign-in in progress (it ended or expired): start again".into(),
            ));
        };
        let http_client = self.http_client().await?;
        let oidc = sso::oidc_client(
            &login.portal.region,
            http_client,
            self.creds_hooks.sso_oidc.as_ref(),
        );
        let outcome = sso::login::poll_login(&oidc, &mut login, Utc::now()).await;
        match outcome {
            Ok(Poll::Pending) => {
                let interval_secs = login.interval.as_secs();
                self.sso_logins
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .pending
                    .insert(login_id.to_owned(), (started, login));
                Ok(SsoLoginPoll::Pending { interval_secs })
            }
            Ok(Poll::Complete(token)) => {
                let dir = self.aws_files().sso_cache().to_path_buf();
                let key = login.portal.cache_key();
                let expires_at = token.expires_at.to_rfc3339_opts(SecondsFormat::Secs, true);
                super::blocking(move || sso::cache::write_token(&dir, &key, &token)).await?;
                Ok(SsoLoginPoll::Complete { expires_at })
            }
            Err(error) => {
                if error.kind() != crate::ErrorKind::Auth {
                    self.sso_logins
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .pending
                        .insert(login_id.to_owned(), (started, login));
                }
                Err(error)
            }
        }
    }

    /// Forgets a sign-in in progress (the user closed the dialog).
    pub fn sso_cancel_login(&self, login_id: &str) {
        self.sso_logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .remove(login_id);
    }

    /// Whether a portal has a usable sign-in in the AWS CLI cache.
    pub async fn sso_status(
        &self,
        start_url: &str,
        sso_region: &str,
        sso_session: Option<&str>,
    ) -> Result<SignInStatus> {
        let portal = Portal::new(start_url, sso_region, sso_session)?;
        let dir = self.aws_files().sso_cache().to_path_buf();
        super::blocking(move || sso::sign_in_status(&dir, &portal, Utc::now())).await
    }

    /// Deletes the cached sign-in of a portal.
    pub async fn sso_sign_out(
        &self,
        start_url: &str,
        sso_region: &str,
        sso_session: Option<&str>,
    ) -> Result<()> {
        let portal = Portal::new(start_url, sso_region, sso_session)?;
        let dir = self.aws_files().sso_cache().to_path_buf();
        super::blocking(move || sso::cache::remove_token(&dir, &portal.cache_key())).await
    }

    /// The accounts the signed-in user can reach through a portal.
    pub async fn sso_list_accounts(
        &self,
        start_url: &str,
        sso_region: &str,
        sso_session: Option<&str>,
    ) -> Result<Vec<SsoAccount>> {
        let (portal_api, token) = self.sso_api(start_url, sso_region, sso_session).await?;
        sso::list_accounts(&portal_api, &token).await
    }

    /// The roles of one AWS account the signed-in user can use.
    pub async fn sso_list_roles(
        &self,
        start_url: &str,
        sso_region: &str,
        sso_session: Option<&str>,
        account_id: &str,
    ) -> Result<Vec<String>> {
        let (portal_api, token) = self.sso_api(start_url, sso_region, sso_session).await?;
        sso::list_roles(&portal_api, &token, account_id).await
    }

    async fn sso_api(
        &self,
        start_url: &str,
        sso_region: &str,
        sso_session: Option<&str>,
    ) -> Result<(aws_sdk_sso::Client, sso::SsoToken)> {
        let portal = Portal::new(start_url, sso_region, sso_session)?;
        let http_client = self.http_client().await?;
        let oidc = sso::oidc_client(
            sso_region,
            http_client.clone(),
            self.creds_hooks.sso_oidc.as_ref(),
        );
        let portal_api = sso::portal_client(sso_region, http_client, self.creds_hooks.sso.as_ref());
        let dir = self.aws_files().sso_cache().to_path_buf();
        let token = sso::usable_token(&oidc, &portal, &dir, Utc::now()).await?;
        Ok((portal_api, token))
    }
}
