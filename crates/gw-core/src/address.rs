//! Session addressing for the CLI: canonical `provider:session-id`, or a
//! bare session id / unique id prefix (≥ 4 chars). Scope is honest: only
//! Sessions gw has observed via hooks resolve — not the provider's full
//! universe. Ambiguity is an error, never a silent first match.

use anyhow::{bail, Result};

use crate::store::SessionRecord;

/// Minimum length for prefix matching; shorter inputs must match exactly.
pub const MIN_PREFIX: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub provider: String,
    pub session: String,
}

impl Address {
    pub fn canonical(&self) -> String {
        format!("{}:{}", self.provider, self.session)
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.provider, self.session)
    }
}

/// Resolve `input` against the known session records. Accepts
/// `provider:session-id`, `provider:prefix`, a bare id, or a bare unique
/// prefix. Exact id matches win over prefix matches.
pub fn resolve<'a>(input: &str, sessions: &'a [SessionRecord]) -> Result<&'a SessionRecord> {
    let input = input.trim();
    if input.is_empty() {
        bail!("empty session address");
    }
    let (provider, id) = match input.split_once(':') {
        Some((provider, id)) if !provider.is_empty() && !id.is_empty() => (Some(provider), id),
        Some(_) => bail!("malformed address {input:?}: expected provider:session-id"),
        None => (None, input),
    };

    let known = |session: &&SessionRecord| {
        !session.meta.session.is_empty()
            && provider.is_none_or(|provider| session.meta.provider == provider)
    };
    let exact: Vec<&SessionRecord> = sessions
        .iter()
        .filter(known)
        .filter(|session| session.meta.session == id)
        .collect();
    match exact.as_slice() {
        [session] => return Ok(session),
        [] => {}
        many => bail!("ambiguous session {input:?}: matches {}", candidates(many)),
    }

    if id.len() < MIN_PREFIX {
        bail!("no session matching {input:?} (prefixes need at least {MIN_PREFIX} characters)");
    }
    let prefixed: Vec<&SessionRecord> = sessions
        .iter()
        .filter(known)
        .filter(|session| session.meta.session.starts_with(id))
        .collect();
    match prefixed.as_slice() {
        [session] => Ok(session),
        [] => bail!("no session matching {input:?}"),
        many => bail!("ambiguous session {input:?}: matches {}", candidates(many)),
    }
}

fn candidates(sessions: &[&SessionRecord]) -> String {
    sessions
        .iter()
        .map(|session| format!("{}:{}", session.meta.provider, session.meta.session))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use crate::store::SessionMeta;

    use super::*;

    fn record(provider: &str, session: &str) -> SessionRecord {
        SessionRecord {
            meta: SessionMeta {
                provider: provider.into(),
                session: session.into(),
                pane_id: None,
                pid: None,
                cwd: None,
                transcript_path: None,
                updated_at: Utc::now(),
            },
            events: Vec::new(),
        }
    }

    fn addr(record: &SessionRecord) -> String {
        format!("{}:{}", record.meta.provider, record.meta.session)
    }

    #[test]
    fn resolves_canonical_bare_and_prefix_forms() {
        let sessions = [
            record("claude", "279b0f33-aaaa"),
            record("amp", "T-0199-bbbb"),
        ];

        for input in [
            "claude:279b0f33-aaaa",
            "279b0f33-aaaa",
            "279b",
            "claude:279b",
        ] {
            assert_eq!(
                addr(resolve(input, &sessions).unwrap()),
                "claude:279b0f33-aaaa",
                "input {input:?}"
            );
        }
        assert_eq!(
            addr(resolve("T-0199-bbbb", &sessions).unwrap()),
            "amp:T-0199-bbbb"
        );
    }

    #[test]
    fn exact_match_beats_a_longer_sibling() {
        // "abcd" is both a full id and a prefix of "abcdef".
        let sessions = [record("claude", "abcd"), record("claude", "abcdef")];
        assert_eq!(addr(resolve("abcd", &sessions).unwrap()), "claude:abcd");
    }

    #[test]
    fn ambiguity_is_an_error_naming_candidates() {
        let sessions = [record("claude", "abcdef-1"), record("codex", "abcdef-2")];
        let error = resolve("abcd", &sessions).unwrap_err().to_string();
        assert!(error.contains("ambiguous"), "{error}");
        assert!(error.contains("claude:abcdef-1"), "{error}");
        assert!(error.contains("codex:abcdef-2"), "{error}");

        // A provider qualifier disambiguates.
        assert_eq!(
            addr(resolve("codex:abcd", &sessions).unwrap()),
            "codex:abcdef-2"
        );

        // The same id under two providers needs the qualifier.
        let twins = [record("claude", "same-id"), record("codex", "same-id")];
        assert!(resolve("same-id", &twins).is_err());
        assert_eq!(
            addr(resolve("claude:same-id", &twins).unwrap()),
            "claude:same-id"
        );
    }

    #[test]
    fn unknown_short_and_malformed_inputs_are_errors() {
        let sessions = [record("claude", "abcdef")];
        assert!(resolve("nope", &sessions).is_err());
        // Below the prefix floor, only exact ids match.
        assert!(resolve("abc", &sessions).is_err());
        assert!(resolve("", &sessions).is_err());
        assert!(resolve(":abcdef", &sessions).is_err());
        assert!(resolve("claude:", &sessions).is_err());
        // Wrong provider qualifier.
        assert!(resolve("codex:abcdef", &sessions).is_err());
    }
}
