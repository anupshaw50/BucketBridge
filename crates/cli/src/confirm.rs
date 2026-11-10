//! Confirmations before commands that delete data for good (`bb rm
//! --versions`, `bb sync --delete`): `--yes` skips the question, a terminal
//! is asked, and without either the command refuses.

use bucketbridge_core::{Error, Result};

use crate::context::Context;
use crate::i18n::{tr, trf};
use crate::output::Output;

/// Refuses when a confirmation is needed (`yes` is not set) and there is no
/// terminal to ask on. `what` says what would be deleted, for example
/// "deleting versions". Call it before the expensive part of a command.
pub fn require_answer(ctx: &Context, yes: bool, what: &str) -> Result<()> {
    if yes || ctx.can_ask {
        return Ok(());
    }
    Err(Error::Usage(trf(
        "{what} needs confirmation, and there is no terminal to ask; pass --yes",
        &[("what", what)],
    )))
}

/// Shows `details` (one line each) and asks `question` on the terminal.
/// `yes` skips both. Anything but a yes is [`Error::Cancelled`]: nothing has
/// been changed at that point.
pub fn confirm(
    ctx: &Context,
    out: &Output,
    yes: bool,
    details: &[String],
    question: &str,
) -> Result<()> {
    if yes {
        return Ok(());
    }
    for line in details {
        out.notice(line);
    }
    let prompt = format!("{question} {} ", tr("[y/N]"));
    match ctx.console.confirm(&prompt) {
        Ok(Some(true)) => Ok(()),
        Ok(Some(false)) => Err(Error::Cancelled),
        Ok(None) => Err(Error::Usage(tr(
            "there is no terminal to ask for confirmation; pass --yes",
        ))),
        Err(err) => Err(Error::io(tr("reading the answer"), err)),
    }
}
