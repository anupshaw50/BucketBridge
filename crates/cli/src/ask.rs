//! Asking what to do about an existing destination (`--if-exists ask`) on
//! the terminal.

use std::io::{BufRead, Write};

use bucketbridge_core::engine::{Choice, Conflict, ConflictResolver, ItemInfo};
use bucketbridge_core::{Error, Result};
use futures::future::BoxFuture;
use tokio::sync::Mutex;

/// Asks on standard error and reads the answer from standard input, one
/// question at a time. A capital letter applies the answer to every later
/// conflict of the command.
pub struct TerminalResolver {
    remembered: Mutex<Option<Choice>>,
}

impl TerminalResolver {
    /// A resolver that has not been told anything yet.
    pub fn new() -> Self {
        TerminalResolver {
            remembered: Mutex::new(None),
        }
    }
}

/// An answer typed by the user: the choice, and whether it covers the rest.
/// `None` for a line that is no answer, `Some(Err)` for a cancel.
pub fn parse_answer(line: &str) -> Option<std::result::Result<(Choice, bool), ()>> {
    let letter = line.trim().chars().next()?;
    let all = letter.is_ascii_uppercase();
    let choice = match letter.to_ascii_lowercase() {
        'o' => Choice::Overwrite,
        's' => Choice::Skip,
        'r' => Choice::Rename,
        'n' => Choice::OverwriteIfNewer,
        'c' | 'q' => return Some(Err(())),
        _ => return None,
    };
    Some(Ok((choice, all)))
}

fn describe(info: &ItemInfo) -> String {
    let size = info.size.map_or_else(
        || crate::i18n::tr("unknown size"),
        |s| crate::i18n::trf("{bytes} bytes", &[("bytes", s.to_string().as_str())]),
    );
    let time = info
        .modified
        .map_or_else(|| crate::i18n::tr("unknown time"), |t| t.to_rfc3339());
    crate::i18n::trf("{size}, {time}", &[("size", size.as_str()), ("time", time.as_str())])
}

impl ConflictResolver for TerminalResolver {
    fn resolve<'a>(&'a self, conflict: &'a Conflict) -> BoxFuture<'a, Result<Choice>> {
        Box::pin(async move {
            let mut remembered = self.remembered.lock().await;
            if let Some(choice) = *remembered {
                return Ok(choice);
            }
            let prompt = crate::i18n::trf(
                "\n{destination} already exists ({destination_info}); source {source} ({source_info}).\n[o]verwrite, [s]kip, [r]ename, [n]ewer only, [c]ancel; a capital letter applies to all remaining: ",
                &[
                    ("destination", conflict.destination.as_str()),
                    ("destination_info", describe(&conflict.destination_info).as_str()),
                    ("source", conflict.source.as_str()),
                    ("source_info", describe(&conflict.source_info).as_str()),
                ],
            );
            loop {
                let text = prompt.clone();
                let line = tokio::task::spawn_blocking(move || {
                    let mut stderr = std::io::stderr();
                    let _ = stderr.write_all(text.as_bytes());
                    let _ = stderr.flush();
                    let mut line = String::new();
                    std::io::stdin()
                        .lock()
                        .read_line(&mut line)
                        .map(|n| (n, line))
                })
                .await
                .map_err(|err| Error::Internal(crate::i18n::trf("reading the answer failed: {error}", &[("error", &err.to_string())])))?
                .map_err(|err| Error::io(crate::i18n::tr("reading the answer"), err))?;
                if line.0 == 0 {
                    return Err(Error::Cancelled);
                }
                match parse_answer(&line.1) {
                    Some(Ok((choice, all))) => {
                        if all {
                            *remembered = Some(choice);
                        }
                        return Ok(choice);
                    }
                    Some(Err(())) => return Err(Error::Cancelled),
                    None => {}
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_one_letter_and_capitals_cover_the_rest() {
        assert_eq!(parse_answer("o\n"), Some(Ok((Choice::Overwrite, false))));
        assert_eq!(parse_answer("S"), Some(Ok((Choice::Skip, true))));
        assert_eq!(parse_answer(" rename "), Some(Ok((Choice::Rename, false))));
        assert_eq!(
            parse_answer("N"),
            Some(Ok((Choice::OverwriteIfNewer, true)))
        );
        assert_eq!(parse_answer("c"), Some(Err(())));
        assert_eq!(parse_answer("x"), None);
        assert_eq!(parse_answer("\n"), None);
    }
}
