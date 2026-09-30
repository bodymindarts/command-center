//! "Did you mean?" helpers for user-supplied names.
//!
//! Spawn arguments (project, skill) are free-form strings. When one does not
//! resolve, the operator almost always made a typo or used a truncation of a
//! real name, so the error carries both the full candidate list and the
//! closest match instead of a bare "not found".

/// Minimum similarity for a candidate to be worth suggesting.
const SIMILARITY_THRESHOLD: f64 = 0.5;

/// The candidate closest to `input`, if one is close enough to be a plausible
/// typo or truncation of it. Matching is case-insensitive.
pub fn nearest<'a, I>(input: &str, candidates: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let needle = input.to_lowercase();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let score = similarity(&needle, &candidate.to_lowercase());
            (score >= SIMILARITY_THRESHOLD).then_some((candidate, score))
        })
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(candidate, _)| candidate)
}

/// Similarity of two lowercased names, in `0.0..=1.0`.
///
/// Plain edit distance underrates prefix matches ('lana' vs 'lana-payments'),
/// where the unwritten suffix dominates the distance, so those are scored
/// separately and ranked by how much of the candidate was actually typed.
fn similarity(needle: &str, candidate: &str) -> f64 {
    if needle.starts_with(candidate) || candidate.starts_with(needle) {
        let (short, long) = (
            needle.len().min(candidate.len()),
            needle.len().max(candidate.len()),
        );
        return 0.75 + 0.25 * (short as f64 / long.max(1) as f64);
    }
    strsim::normalized_levenshtein(needle, candidate)
}

/// Error for a name that did not resolve, listing what would have worked and
/// pointing at the closest match. `kind` is a singular noun ("project").
pub fn unknown_name_error(kind: &str, name: &str, mut known: Vec<String>) -> anyhow::Error {
    known.sort();
    if known.is_empty() {
        return anyhow::anyhow!("unknown {kind} '{name}': no {kind}s are defined");
    }
    let suggestion = match nearest(name, known.iter().map(String::as_str)) {
        Some(closest) => format!(" — did you mean '{closest}'?"),
        None => String::new(),
    };
    anyhow::anyhow!(
        "unknown {kind} '{name}'{suggestion}\nknown {kind}s: {}",
        known.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        ["lana-payments", "command-center", "poker"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn suggests_prefix_match() {
        assert_eq!(
            nearest("lana", known().iter().map(String::as_str)),
            Some("lana-payments")
        );
    }

    #[test]
    fn suggests_typo_match() {
        assert_eq!(
            nearest("comand-center", known().iter().map(String::as_str)),
            Some("command-center")
        );
    }

    #[test]
    fn ignores_case() {
        assert_eq!(
            nearest("POKR", known().iter().map(String::as_str)),
            Some("poker")
        );
    }

    #[test]
    fn no_suggestion_when_nothing_is_close() {
        assert_eq!(
            nearest("zzzzzzzzzz", known().iter().map(String::as_str)),
            None
        );
    }

    #[test]
    fn error_lists_known_names_and_suggestion() {
        let err = unknown_name_error("project", "lana", known()).to_string();
        assert!(err.contains("unknown project 'lana'"), "{err}");
        assert!(err.contains("did you mean 'lana-payments'?"), "{err}");
        assert!(err.contains("command-center"), "{err}");
    }

    #[test]
    fn error_without_candidates_says_none_defined() {
        let err = unknown_name_error("skill", "engineer", vec![]).to_string();
        assert!(err.contains("no skills are defined"), "{err}");
    }
}
