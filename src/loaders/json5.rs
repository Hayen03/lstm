//! JSON5 deserialization; model construction is shared with the native loader.

use super::common::{Definition, build};
use crate::{err::Result, model::Model};

/// Builds an executable model from a JSON5 object.
pub struct Json5Loader;

impl Json5Loader {
    pub fn load(
        text: &str,
        override_axiom: Option<&str>,
        globals: &str,
        user_seed: Option<&str>,
        freclim: Option<usize>,
    ) -> Result<Model> {
        let source: Definition = ::json5::from_str(text.trim_start_matches('\u{feff}'))?;
        build(source, override_axiom, globals, user_seed, freclim, None)
    }
}
