//! The recipe as it renders.

use crate::stored::StoredRef;
use crate::tone::Tone;

/// The typed sections of a recipe ready to render, produced only by
/// [`normalize`] and never saved. Sections not moved here yet are read by
/// the engine from the stored recipe.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ResolvedRecipe {
    pub tone: Tone,
}

/// Resolves a stored recipe: every value of a moved section as it renders
/// (stored, defaulted, switched off). The camera and file defaults
/// (`rawtonic_export::normalize`) complete the stored recipe before.
pub fn normalize(stored: StoredRef<'_>) -> ResolvedRecipe {
    ResolvedRecipe {
        tone: Tone::resolve(stored),
    }
}
