//! Shared declarative macros for evidence-bearing types.
//!
//! Declared before the domain modules so that every module may implement a redacting `Debug`
//! through one audited definition rather than a hand-written variant per type.

/// Implements a `Debug` that emits only the type name, for types that hold evidence, identity, or
/// provenance and therefore must never print their values to diagnostics.
macro_rules! impl_redacted_debug {
    ($($type:ty),+ $(,)?) => {
        $(
            /// Redacts all fields from diagnostic output for this evidence-bearing type.
            impl std::fmt::Debug for $type {
                /// Emits only the type name and a non-exhaustive marker, never retained values.
                fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    formatter
                        .debug_struct(stringify!($type))
                        .finish_non_exhaustive()
                }
            }
        )+
    };
}
