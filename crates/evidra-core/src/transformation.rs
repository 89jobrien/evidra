//! Deterministic transformation names applied while producing harness evidence.
//!
//! Names are namespaced `family:action` so the reduction log is greppable and so ADR-010 can
//! union names across evidence without ambiguity. The families defined here are the ones core
//! owns; adapter-specific names belong to the adapter that performs the reduction.

/// Field or record class withheld before any content inspection.
pub const DROP: &str = "drop";

/// Content redaction performed by an engine, such as `obfsck`.
pub const OBFUSCATE: &str = "obfuscate";

/// Returns whether `name` follows the required `family:action` convention.
///
/// A transformation name must carry both halves so that a reader can tell *which stage* performed
/// the reduction without consulting the producer implementation, and so that a family prefix can be
/// audited mechanically across a whole ledger.
#[must_use]
pub fn is_well_formed(name: &str) -> bool {
    let Some((family, action)) = name.split_once(':') else {
        return false;
    };
    !family.trim().is_empty() && !action.trim().is_empty() && !name.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::is_well_formed;

    #[test]
    fn namespaced_names_are_accepted() {
        assert!(is_well_formed("drop:tool_use.input"));
        assert!(is_well_formed("obfuscate:email"));
        assert!(is_well_formed("drop:x"));
    }

    #[test]
    fn unnamespaced_names_are_refused() {
        assert!(!is_well_formed("tool_use.input"));
        assert!(!is_well_formed(""));
        assert!(!is_well_formed(":action"));
        assert!(!is_well_formed("family:"));
    }

    #[test]
    fn embedded_whitespace_is_refused() {
        assert!(!is_well_formed("drop:two words"));
        assert!(!is_well_formed("drop :action"));
        assert!(!is_well_formed("drop:action "));
    }

    #[test]
    fn a_second_colon_is_permitted_in_the_action() {
        assert!(is_well_formed("drop:tool_use.input"));
        assert!(is_well_formed("obfuscate:secret:count"));
    }
}
