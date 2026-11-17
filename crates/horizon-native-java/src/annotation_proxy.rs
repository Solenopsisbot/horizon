//! Runtime annotation proxy helpers.
//!
//! Annotation values are currently represented by markers in the native
//! interpreter. This module is kept as the extension point for proxy-specific
//! behavior while the runtime's bounded member dispatch remains in `lib.rs`.
