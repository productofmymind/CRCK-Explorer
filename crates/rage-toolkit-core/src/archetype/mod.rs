//! The archetype (`.ytyp`) editor's library side.
pub mod fidelity;
pub mod edit;
pub mod schema_view;
pub mod document;
pub mod validate;
/// the tree the editor works on, for callers that do not depend on `rage-formats` themselves
pub use rage_formats::MetaValue;
pub mod merge;
