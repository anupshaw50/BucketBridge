//! A guard for pagination loops.

use std::collections::HashSet;
use std::hash::Hash;

use bucketbridge_core::Error;

/// Remembers every continuation token (or marker) a listing has followed
/// and refuses one that comes back. A server that answers with the same
/// token again, or cycles (A, B, A), then ends the listing with a `remote`
/// error instead of paging forever.
#[derive(Debug)]
pub struct SeenTokens<T> {
    seen: HashSet<T>,
}

impl<T> Default for SeenTokens<T> {
    fn default() -> Self {
        SeenTokens {
            seen: HashSet::new(),
        }
    }
}

impl<T: Eq + Hash + Clone> SeenTokens<T> {
    /// Returns `token` when it is new; an error naming `what` when the
    /// server has sent it before.
    pub fn follow(&mut self, token: T, what: &str) -> Result<T, Error> {
        if self.seen.insert(token.clone()) {
            Ok(token)
        } else {
            Err(Error::Remote(format!(
                "{what}: the server returned a continuation token it had already sent; stopping to avoid an endless listing"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bucketbridge_core::ErrorKind;

    #[test]
    fn repeats_and_cycles_are_refused() {
        let mut seen = SeenTokens::default();
        assert_eq!(seen.follow("A".to_owned(), "listing").unwrap(), "A");
        assert_eq!(seen.follow("B".to_owned(), "listing").unwrap(), "B");
        let err = seen.follow("A".to_owned(), "listing s3://b/").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Remote);
        assert!(err.to_string().starts_with("listing s3://b/: "), "{err}");

        let mut seen = SeenTokens::default();
        seen.follow(("k".to_owned(), Some("v".to_owned())), "x")
            .unwrap();
        seen.follow(("k".to_owned(), None), "x").unwrap();
        assert!(
            seen.follow(("k".to_owned(), Some("v".to_owned())), "x")
                .is_err()
        );
    }
}
