//! What an invocation reads from and writes to its process, so that the
//! commands can also run in-process with captured streams.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufRead, IsTerminal, Write};

use bucketbridge_core::{Error, Result};

/// Environment variables of one invocation.
#[derive(Debug, Clone, Default)]
pub struct Env {
    vars: HashMap<String, OsString>,
}

impl Env {
    /// The variables of this process.
    pub fn from_process() -> Self {
        Env {
            vars: std::env::vars_os()
                .filter_map(|(name, value)| name.into_string().ok().map(|name| (name, value)))
                .collect(),
        }
    }

    /// Exactly the given variables.
    pub fn from_pairs<I, K, V>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<OsString>,
    {
        Env {
            vars: pairs
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        }
    }

    /// The raw value of `name`.
    pub fn get_os(&self, name: &str) -> Option<&OsString> {
        self.vars.get(name)
    }

    /// The value of `name`; unset and blank values are `None`, a value that
    /// is not UTF-8 is a usage error.
    pub fn get(&self, name: &str) -> Result<Option<String>> {
        match self.vars.get(name) {
            None => Ok(None),
            Some(value) => match value.to_str() {
                Some(text) if text.trim().is_empty() => Ok(None),
                Some(text) => Ok(Some(text.to_owned())),
                None => Err(Error::Usage(format!("{name} is not valid UTF-8"))),
            },
        }
    }
}

/// The interactive side of the terminal.
pub trait Console: Send + Sync {
    /// Reads a secret from the terminal without echoing it. `Ok(None)` when
    /// there is no terminal to ask.
    fn read_secret(&self, prompt: &str) -> io::Result<Option<String>>;

    /// Whether standard error is a terminal (progress is drawn only there).
    fn stderr_is_terminal(&self) -> bool;
}

/// The console of this process: prompts go to the controlling terminal
/// (not to standard input or output), with echo turned off.
#[derive(Debug, Default)]
pub struct ProcessConsole;

impl Console for ProcessConsole {
    fn read_secret(&self, prompt: &str) -> io::Result<Option<String>> {
        // Without a controlling terminal there is nobody to ask.
        Ok(rpassword::prompt_password(prompt).ok())
    }

    fn stderr_is_terminal(&self) -> bool {
        io::stderr().is_terminal()
    }
}

/// A console without a terminal, for scripts and tests: prompts are never
/// answered and no progress is drawn.
#[derive(Debug, Default)]
pub struct NoConsole;

impl Console for NoConsole {
    fn read_secret(&self, _prompt: &str) -> io::Result<Option<String>> {
        Ok(None)
    }

    fn stderr_is_terminal(&self) -> bool {
        false
    }
}

/// The streams, environment and console of one invocation.
pub struct Io {
    /// Environment variables.
    pub env: Env,
    /// Standard input (`--secret-stdin`).
    pub stdin: Box<dyn BufRead + Send>,
    /// Standard output: data and records.
    pub stdout: Box<dyn Write + Send>,
    /// Standard error: errors, failed items and progress.
    pub stderr: Box<dyn Write + Send>,
    /// Secret prompts and terminal detection.
    pub console: Box<dyn Console>,
}

impl Io {
    /// The real process: its environment, standard streams and terminal.
    pub fn process() -> Self {
        Io {
            env: Env::from_process(),
            stdin: Box::new(io::BufReader::new(io::stdin())),
            stdout: Box::new(io::stdout()),
            stderr: Box::new(io::stderr()),
            console: Box::new(ProcessConsole),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_and_unset_values_are_none() {
        let env = Env::from_pairs([("A", "x"), ("B", "  "), ("C", "")]);
        assert_eq!(env.get("A").unwrap().as_deref(), Some("x"));
        assert_eq!(env.get("B").unwrap(), None);
        assert_eq!(env.get("C").unwrap(), None);
        assert_eq!(env.get("D").unwrap(), None);
        assert!(env.get_os("A").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_values_are_usage_errors() {
        use std::os::unix::ffi::OsStringExt;
        let env = Env::from_pairs([("BAD", OsString::from_vec(vec![0xff, 0xfe]))]);
        let err = env.get("BAD").unwrap_err();
        assert_eq!(err.kind(), bucketbridge_core::ErrorKind::Usage);
    }

    #[test]
    fn no_console_never_answers() {
        assert_eq!(NoConsole.read_secret("x").unwrap(), None);
        assert!(!NoConsole.stderr_is_terminal());
    }
}
