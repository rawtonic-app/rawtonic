//! The recipe as it renders.

use crate::color::Color;
use crate::detail::Detail;
use crate::effects::Effects;
use crate::geometry::Geometry;
use crate::lens::Lens;
use crate::stored::StoredRef;
use crate::tone::Tone;

/// The typed photo-wide sections of a recipe ready to render, produced only
/// by [`normalize`] and never saved. The mask stack is not part of it: the
/// stages that read it resolve it themselves ([`crate::masks::resolve`]),
/// so the others do not pay for it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ResolvedRecipe<'a> {
    pub tone: Tone,
    pub color: Color,
    pub detail: Detail,
    pub geometry: Geometry,
    pub effects: Effects<'a>,
    pub lens: Lens,
}

/// Resolves a stored recipe: every value of a moved section as it renders
/// (stored, defaulted, switched off). The camera and file defaults
/// (`rawtonic_export::normalize`) complete the stored recipe before. `is_raw`
/// supplies the capture defaults for detail amounts absent from the recipe.
pub fn normalize(stored: StoredRef<'_>, is_raw: bool) -> ResolvedRecipe<'_> {
    ResolvedRecipe {
        tone: Tone::resolve(stored),
        color: Color::resolve(stored),
        detail: Detail::resolve(stored, is_raw),
        geometry: Geometry::resolve(stored),
        effects: Effects::resolve(stored),
        lens: Lens::resolve(stored),
    }
}
