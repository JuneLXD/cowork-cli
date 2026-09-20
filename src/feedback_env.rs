//! Which feedback endpoint pair applies, shared by build.rs and the runtime so the
//! two cannot drift. The rule: a primary (COWORK_) pair that is only half set is an
//! error, never a fallback; the legacy (ROOM_) pair is consulted only when neither
//! primary value is set, and it too must be complete.

#[allow(dead_code)]
pub fn resolve_pair(
    primary: (Option<String>, Option<String>),
    legacy: (Option<String>, Option<String>),
) -> Result<Option<(String, String, &'static str)>, String> {
    let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let (pu, pk) = (clean(primary.0), clean(primary.1));
    match (pu, pk) {
        (Some(u), Some(k)) => return Ok(Some((u, k, "COWORK_FEEDBACK_URL / COWORK_FEEDBACK_KEY"))),
        (None, None) => {}
        (u, _) => {
            let missing = if u.is_none() { "COWORK_FEEDBACK_URL" } else { "COWORK_FEEDBACK_KEY" };
            return Err(format!("COWORK_FEEDBACK_URL and COWORK_FEEDBACK_KEY must be set together; {missing} is missing. Set both, or unset both to use the next source."));
        }
    }
    let (lu, lk) = (clean(legacy.0), clean(legacy.1));
    match (lu, lk) {
        (Some(u), Some(k)) => Ok(Some((u, k, "ROOM_FEEDBACK_URL / ROOM_FEEDBACK_KEY (legacy names)"))),
        (None, None) => Ok(None),
        (u, _) => {
            let missing = if u.is_none() { "ROOM_FEEDBACK_URL" } else { "ROOM_FEEDBACK_KEY" };
            Err(format!("ROOM_FEEDBACK_URL and ROOM_FEEDBACK_KEY must be set together; {missing} is missing. Set both, or unset both to use the next source."))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_pair;
    fn s(v: &str) -> Option<String> {
        Some(v.to_string())
    }

    #[test]
    fn complete_primary_wins_over_legacy() {
        let r = resolve_pair((s("https://p"), s("sb_publishable_p")), (s("https://l"), s("sb_publishable_l"))).unwrap().unwrap();
        assert_eq!(r.0, "https://p");
    }

    #[test]
    fn partial_primary_is_an_error_even_with_a_complete_legacy_pair() {
        let e = resolve_pair((s("https://p"), None), (s("https://l"), s("sb_publishable_l"))).unwrap_err();
        assert!(e.contains("COWORK_FEEDBACK_KEY is missing"), "{e}");
        let e = resolve_pair((None, s("sb_publishable_p")), (s("https://l"), s("sb_publishable_l"))).unwrap_err();
        assert!(e.contains("COWORK_FEEDBACK_URL is missing"), "{e}");
        let e = resolve_pair((s("  "), s("k")), (None, None)).unwrap_err();
        assert!(e.contains("COWORK_FEEDBACK_URL is missing"), "blank counts as unset: {e}");
    }

    #[test]
    fn legacy_pair_applies_only_when_primary_is_absent_and_must_be_complete() {
        let r = resolve_pair((None, None), (s("https://l"), s("sb_publishable_l"))).unwrap().unwrap();
        assert_eq!(r.0, "https://l");
        assert!(r.2.contains("legacy"));
        let e = resolve_pair((None, None), (s("https://l"), None)).unwrap_err();
        assert!(e.contains("ROOM_FEEDBACK_KEY is missing"), "{e}");
        assert!(resolve_pair((None, None), (None, None)).unwrap().is_none());
    }
}
