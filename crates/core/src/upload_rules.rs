//! Upload rules: values that an upload gets by file pattern.
//!
//! An ordered list of rules lives in the settings ([`UploadRulesSettings`]).
//! Each rule has a glob pattern and the values it sets on the object:
//! `Content-Type`, `Cache-Control`, `Content-Disposition`,
//! `Content-Encoding`, other headers (`Content-Language`, `Expires`, user
//! metadata as `x-amz-meta-<name>`), a storage class and server-side
//! encryption. The first enabled rule whose pattern matches wins; values the
//! caller set explicitly for a transfer are never replaced by a rule.
//!
//! A pattern without `/` is matched against the file name (the last part of
//! the object key), so `*.jpg` matches `a/b/photo.jpg`. A pattern with `/` is
//! matched against the whole key (`**/*.min.js`, `assets/*.css`).

use std::collections::BTreeMap;

use globset::{GlobBuilder, GlobMatcher};
use serde::{Deserialize, Serialize};

use crate::engine::UploadHeaders;
use crate::error::{Error, Result};

/// Most rules a list may hold.
pub const MAX_UPLOAD_RULES: usize = 200;

/// The upload rules of the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadRulesSettings {
    /// Apply the rules to uploads. Default `true` (a new list is empty).
    pub enabled: bool,
    /// The rules, in order; the first match wins.
    pub rules: Vec<UploadRule>,
}

impl Default for UploadRulesSettings {
    fn default() -> Self {
        UploadRulesSettings {
            enabled: true,
            rules: Vec::new(),
        }
    }
}

/// One upload rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadRule {
    /// A label for the list; may be empty.
    pub name: String,
    /// Switch a rule off without deleting it. Default `true`.
    pub enabled: bool,
    /// Glob pattern (`*`, `**`, `?`, `[a-z]`, `{a,b}`).
    pub pattern: String,
    /// Match the pattern without regard to case. Default `true`.
    pub case_insensitive: bool,
    /// `Content-Type`.
    pub content_type: Option<String>,
    /// `Cache-Control`.
    pub cache_control: Option<String>,
    /// `Content-Disposition`.
    pub content_disposition: Option<String>,
    /// `Content-Encoding`.
    pub content_encoding: Option<String>,
    /// Other headers: `Content-Language`, `Expires` (an HTTP date) and user
    /// metadata (`x-amz-meta-<name>`). Header names are case-insensitive.
    pub headers: BTreeMap<String, String>,
    /// Storage class (`STANDARD_IA`, `GLACIER_IR`, ...).
    pub storage_class: Option<String>,
    /// Server-side encryption: `AES256`, `aws:kms` or `aws:kms:dsse`.
    pub sse: Option<String>,
    /// The KMS key (ID, ARN or alias) for `aws:kms`; the default key when
    /// empty.
    pub sse_kms_key_id: Option<String>,
}

impl Default for UploadRule {
    fn default() -> Self {
        UploadRule {
            name: String::new(),
            enabled: true,
            pattern: String::new(),
            case_insensitive: true,
            content_type: None,
            cache_control: None,
            content_disposition: None,
            content_encoding: None,
            headers: BTreeMap::new(),
            storage_class: None,
            sse: None,
            sse_kms_key_id: None,
        }
    }
}

/// What a rule test found.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct UploadRuleMatch {
    /// Position of the matching rule in the list; `None` when no rule matches.
    pub index: Option<usize>,
    /// The matching rule.
    pub rule: Option<UploadRule>,
}

impl UploadRule {
    /// Checks the rule; `at` names it in the messages (`upload_rules.rules[2]`).
    pub fn validate(&self, at: &str) -> Result<()> {
        let bad = |what: String| Err(Error::Usage(format!("{at}: {what}")));
        if self.pattern.trim().is_empty() {
            return bad("the pattern is empty".to_owned());
        }
        if let Err(err) = matcher(&self.pattern, self.case_insensitive) {
            return bad(format!(
                "the pattern '{}' is not valid: {err}",
                self.pattern
            ));
        }
        let values = [
            ("content_type", &self.content_type),
            ("cache_control", &self.cache_control),
            ("content_disposition", &self.content_disposition),
            ("content_encoding", &self.content_encoding),
        ];
        for (field, value) in values {
            if let Some(value) = value
                && !header_value_ok(value)
            {
                return bad(format!("{field} is empty or has a line break"));
            }
        }
        if let Some(value) = &self.content_type
            && !value.contains('/')
        {
            return bad("content_type must look like type/subtype".to_owned());
        }
        for (name, value) in &self.headers {
            match name.to_ascii_lowercase().as_str() {
                "content-language" => {}
                "expires" => {
                    if parse_http_date(value).is_none() {
                        return bad("the Expires header must be an HTTP date".to_owned());
                    }
                }
                other => match other.strip_prefix("x-amz-meta-") {
                    Some(key)
                        if !key.is_empty()
                            && key.bytes().all(|b| b.is_ascii_graphic() && b != b':') => {}
                    _ => {
                        return bad(format!(
                            "the header '{name}' is not supported; use Content-Language, Expires or x-amz-meta-<name>"
                        ));
                    }
                },
            }
            if !header_value_ok(value) {
                return bad(format!(
                    "the value of '{name}' is empty or has a line break"
                ));
            }
        }
        if let Some(class) = &self.storage_class
            && (class.is_empty()
                || !class
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
        {
            return bad(format!(
                "the storage class '{class}' must be upper case letters, digits and underscores"
            ));
        }
        if let Some(sse) = &self.sse
            && !matches!(sse.as_str(), "AES256" | "aws:kms" | "aws:kms:dsse")
        {
            return bad(format!(
                "the encryption '{sse}' must be AES256, aws:kms or aws:kms:dsse"
            ));
        }
        if self
            .sse_kms_key_id
            .as_deref()
            .is_some_and(|k| !k.is_empty())
            && !self
                .sse
                .as_deref()
                .is_some_and(|s| s.starts_with("aws:kms"))
        {
            return bad("a KMS key needs the encryption aws:kms".to_owned());
        }
        Ok(())
    }

    /// The headers this rule sets, with `base` taking precedence: a value
    /// that `base` already has is kept.
    pub fn fill(&self, base: &UploadHeaders) -> UploadHeaders {
        let mut out = base.clone();
        fn keep(slot: &mut Option<String>, rule: &Option<String>) {
            if slot.is_none() {
                slot.clone_from(rule);
            }
        }
        keep(&mut out.cache_control, &self.cache_control);
        keep(&mut out.content_disposition, &self.content_disposition);
        keep(&mut out.content_encoding, &self.content_encoding);
        keep(&mut out.storage_class, &self.storage_class);
        if out.server_side_encryption.is_none() {
            out.server_side_encryption.clone_from(&self.sse);
            out.sse_kms_key_id = self.sse_kms_key_id.clone().filter(|k| !k.is_empty());
        }
        for (name, value) in &self.headers {
            let name = name.to_ascii_lowercase();
            match name.as_str() {
                "content-language" => keep(&mut out.content_language, &Some(value.clone())),
                "expires" => {
                    if out.expires.is_none() {
                        out.expires = parse_http_date(value);
                    }
                }
                _ => {
                    if let Some(key) = name.strip_prefix("x-amz-meta-") {
                        out.metadata
                            .entry(key.to_owned())
                            .or_insert_with(|| value.clone());
                    }
                }
            }
        }
        out
    }
}

fn header_value_ok(value: &str) -> bool {
    !value.trim().is_empty() && !value.contains(['\r', '\n'])
}

fn parse_http_date(value: &str) -> Option<aws_smithy_types::DateTime> {
    aws_smithy_types::DateTime::from_str(
        value.trim(),
        aws_smithy_types::date_time::Format::HttpDate,
    )
    .ok()
}

fn matcher(
    pattern: &str,
    case_insensitive: bool,
) -> std::result::Result<GlobMatcher, globset::Error> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .case_insensitive(case_insensitive)
        .build()
        .map(|glob| glob.compile_matcher())
}

/// One compiled rule.
#[derive(Debug, Clone)]
struct Compiled {
    index: usize,
    matcher: GlobMatcher,
    whole_key: bool,
    rule: UploadRule,
}

/// The rules of [`UploadRulesSettings`], compiled for matching.
#[derive(Debug, Clone, Default)]
pub struct UploadRules {
    rules: Vec<Compiled>,
}

impl UploadRules {
    /// Compiles the enabled rules (none when the list is switched off).
    /// An invalid rule is a usage error.
    pub fn compile(settings: &UploadRulesSettings) -> Result<Self> {
        validate_list(settings)?;
        let mut rules = Vec::new();
        if settings.enabled {
            for (index, rule) in settings.rules.iter().enumerate() {
                if !rule.enabled {
                    continue;
                }
                let matcher = matcher(&rule.pattern, rule.case_insensitive)
                    .map_err(|err| Error::Usage(err.to_string()))?;
                rules.push(Compiled {
                    index,
                    matcher,
                    whole_key: rule.pattern.contains('/'),
                    rule: rule.clone(),
                });
            }
        }
        Ok(UploadRules { rules })
    }

    /// Whether no rule can match.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The first rule that matches the object key `key` (or a file name),
    /// with its position in the settings list.
    pub fn find(&self, key: &str) -> Option<(usize, &UploadRule)> {
        let name = key.rsplit('/').next().unwrap_or(key);
        self.rules
            .iter()
            .find(|c| c.matcher.is_match(if c.whole_key { key } else { name }))
            .map(|c| (c.index, &c.rule))
    }
}

/// Checks a whole list.
pub fn validate_list(settings: &UploadRulesSettings) -> Result<()> {
    if settings.rules.len() > MAX_UPLOAD_RULES {
        return Err(Error::Usage(format!(
            "upload_rules.rules: at most {MAX_UPLOAD_RULES} rules"
        )));
    }
    for (index, rule) in settings.rules.iter().enumerate() {
        rule.validate(&format!("upload_rules.rules[{index}]"))?;
    }
    Ok(())
}

/// Which rule of `settings` applies to `name` (an object key or file name),
/// whether or not the list is switched on.
pub fn test_rules(settings: &UploadRulesSettings, name: &str) -> Result<UploadRuleMatch> {
    let compiled = UploadRules::compile(&UploadRulesSettings {
        enabled: true,
        rules: settings.rules.clone(),
    })?;
    Ok(match compiled.find(name) {
        Some((index, rule)) => UploadRuleMatch {
            index: Some(index),
            rule: Some(rule.clone()),
        },
        None => UploadRuleMatch::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorKind;

    fn rule(pattern: &str) -> UploadRule {
        UploadRule {
            pattern: pattern.to_owned(),
            ..UploadRule::default()
        }
    }

    fn list(rules: Vec<UploadRule>) -> UploadRules {
        UploadRules::compile(&UploadRulesSettings {
            enabled: true,
            rules,
        })
        .unwrap()
    }

    #[test]
    fn the_first_match_wins_and_names_match_by_file_name() {
        let mut a = rule("*.jpg");
        a.cache_control = Some("max-age=1".into());
        let mut b = rule("*");
        b.cache_control = Some("no-cache".into());
        let rules = list(vec![a, b]);
        assert_eq!(rules.find("x/y/photo.jpg").unwrap().0, 0);
        assert_eq!(rules.find("x/y/readme.txt").unwrap().0, 1);
    }

    #[test]
    fn case_is_ignored_unless_asked() {
        let rules = list(vec![rule("*.jpg")]);
        assert!(rules.find("A.JPG").is_some());
        let mut strict = rule("*.jpg");
        strict.case_insensitive = false;
        let rules = list(vec![strict]);
        assert!(rules.find("A.JPG").is_none());
        assert!(rules.find("a.jpg").is_some());
    }

    #[test]
    fn patterns_with_a_slash_match_the_whole_key() {
        let rules = list(vec![rule("assets/*.css"), rule("**/*.min.js")]);
        assert_eq!(rules.find("assets/site.css").unwrap().0, 0);
        assert!(rules.find("other/assets/site.css").is_none());
        assert_eq!(rules.find("a/b/app.min.js").unwrap().0, 1);
        assert!(rules.find("assets/deep/site.css").is_none());
    }

    #[test]
    fn disabled_rules_and_lists_never_match() {
        let mut off = rule("*");
        off.enabled = false;
        assert!(list(vec![off.clone()]).find("a").is_none());
        let rules = UploadRules::compile(&UploadRulesSettings {
            enabled: false,
            rules: vec![rule("*")],
        })
        .unwrap();
        assert!(rules.is_empty());
        // A disabled rule keeps its position number in the list.
        assert_eq!(list(vec![off, rule("*")]).find("a").unwrap().0, 1);
    }

    #[test]
    fn a_rule_fills_only_what_the_caller_left_open() {
        let mut r = rule("*");
        r.cache_control = Some("max-age=60".into());
        r.storage_class = Some("STANDARD_IA".into());
        r.sse = Some("aws:kms".into());
        r.sse_kms_key_id = Some("alias/x".into());
        r.headers.insert("X-Amz-Meta-Team".into(), "web".into());
        r.headers.insert("Content-Language".into(), "en".into());
        r.headers
            .insert("Expires".into(), "Wed, 21 Oct 2026 07:28:00 GMT".into());
        r.validate("r").unwrap();

        let mut base = UploadHeaders {
            cache_control: Some("no-store".into()),
            ..UploadHeaders::default()
        };
        base.metadata.insert("team".into(), "mine".into());
        let out = r.fill(&base);
        assert_eq!(out.cache_control.as_deref(), Some("no-store"));
        assert_eq!(out.storage_class.as_deref(), Some("STANDARD_IA"));
        assert_eq!(out.server_side_encryption.as_deref(), Some("aws:kms"));
        assert_eq!(out.sse_kms_key_id.as_deref(), Some("alias/x"));
        assert_eq!(out.metadata["team"], "mine");
        assert_eq!(out.content_language.as_deref(), Some("en"));
        assert!(out.expires.is_some());

        let out = r.fill(&UploadHeaders::default());
        assert_eq!(out.cache_control.as_deref(), Some("max-age=60"));
        assert_eq!(out.metadata["team"], "web");
    }

    #[test]
    fn invalid_rules_are_usage_errors() {
        let mut cases = vec![rule(""), rule("[a")];
        let mut r = rule("*");
        r.content_type = Some("nonsense".into());
        cases.push(r);
        let mut r = rule("*");
        r.cache_control = Some("a\nb".into());
        cases.push(r);
        let mut r = rule("*");
        r.headers.insert("Host".into(), "x".into());
        cases.push(r);
        let mut r = rule("*");
        r.headers.insert("Expires".into(), "tomorrow".into());
        cases.push(r);
        let mut r = rule("*");
        r.storage_class = Some("standard ia".into());
        cases.push(r);
        let mut r = rule("*");
        r.sse = Some("rot13".into());
        cases.push(r);
        let mut r = rule("*");
        r.sse = Some("AES256".into());
        r.sse_kms_key_id = Some("k".into());
        cases.push(r);
        for case in cases {
            assert_eq!(
                case.validate("r").unwrap_err().kind(),
                ErrorKind::Usage,
                "{case:?}"
            );
        }
    }

    #[test]
    fn testing_a_list_reports_the_match() {
        let settings = UploadRulesSettings {
            enabled: false,
            rules: vec![rule("*.png"), rule("*.txt")],
        };
        let hit = test_rules(&settings, "dir/a.TXT").unwrap();
        assert_eq!(hit.index, Some(1));
        assert_eq!(
            test_rules(&settings, "a.bin").unwrap(),
            UploadRuleMatch::default()
        );
    }

    #[test]
    fn an_old_settings_table_loads_without_rules() {
        let parsed: UploadRulesSettings = serde_json::from_str("{}").unwrap();
        assert!(parsed.enabled);
        assert!(parsed.rules.is_empty());
        let parsed: UploadRule = serde_json::from_str(r#"{"pattern":"*.a"}"#).unwrap();
        assert!(parsed.case_insensitive && parsed.enabled);
    }
}
