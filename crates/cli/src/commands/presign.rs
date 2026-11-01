//! `bb presign`

use bucketbridge_core::shorten::{Endpoints, provider_name};
use bucketbridge_core::{Error, settings::ShortenerProvider};
use chrono::{TimeDelta, Utc};

use crate::cli::{PresignArgs, PresignMethod};
use crate::context::{Context, open_session};
use crate::output::{Failure, Output};
use crate::records::PresignRecord;

/// Prints a presigned URL (only the URL, unless `--json`).
pub async fn run(ctx: &Context, args: &PresignArgs, out: &mut Output) -> Result<(), Failure> {
    let uri = &args.uri;
    if uri.key().is_empty() {
        return Err(Error::Usage(format!(
            "{uri} names a bucket; presign needs an object, for example s3://{}/key",
            uri.bucket()
        ))
        .into());
    }
    let issued_at = Utc::now();
    let url = match args.method {
        PresignMethod::Get => {
            ctx.s3
                .presign_get(uri.bucket(), uri.key(), args.expires)
                .await?
        }
        PresignMethod::Put => {
            ctx.s3
                .presign_put(uri.bucket(), uri.key(), args.expires)
                .await?
        }
    };
    let lifetime = TimeDelta::from_std(args.expires)
        .map_err(|err| Error::Internal(format!("presign lifetime out of range: {err}")))?;
    let short_url = if args.shorten {
        Some(shorten(ctx, &url, out).await?)
    } else {
        None
    };
    let record = PresignRecord {
        bucket: uri.bucket().to_owned(),
        key: uri.key().to_owned(),
        method: args.method.as_str(),
        expires_secs: args.expires.as_secs(),
        expires_at: issued_at + lifetime,
        url,
        short_url,
    };
    out.row(&record, || {
        record
            .short_url
            .clone()
            .unwrap_or_else(|| record.url.clone())
    })
}

/// Sends the URL to the shortener of the settings (never without
/// `--shorten`) and returns the short link.
async fn shorten(ctx: &Context, url: &str, out: &Output) -> Result<String, Failure> {
    let provider = ctx.settings.shortener.provider;
    if provider == ShortenerProvider::None {
        return Err(Error::Usage(
            "no URL shortener is configured: choose one in the settings (the desktop app's \
             Settings, or `provider` under [shortener] in config.toml)"
                .into(),
        )
        .into());
    }
    let access = &ctx.shortener;
    let session = open_session(ctx.dirs.as_ref(), &access.secrets)?;
    let endpoints = match &access.endpoint {
        Some(endpoint) => Endpoints {
            bitly: endpoint.clone(),
            tinyurl: endpoint.clone(),
            tinyurl_plain: endpoint.clone(),
        },
        None => Endpoints::default(),
    };
    out.notice(&format!(
        "note: the link, including its signature, is sent to {}",
        provider_name(provider)
    ));
    let short = session
        .shorten_url_with(&endpoints, access.token.clone(), url)
        .await?;
    Ok(short)
}
