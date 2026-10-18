//! Live brush frames without the whole recipe. While a stroke is drawn, the
//! webview sends the recipe with the stroke once and then, per pointer move,
//! only the points the stroke gained (`src/utils/liveMaskRender.ts`). The
//! points are appended here on arrival, before the preview slot may drop the
//! request, so a frame that is never rendered still leaves its points behind.
//! A frame's IPC payload then stays small however long the stroke is and
//! however many layers the photo has.

use serde::Deserialize;
use serde_json::Value;

/// The reply to points that do not continue the stroke held here (a frame
/// was lost or overtaken); the webview then sends the recipe again.
pub const LIVE_STROKE_RESYNC: &str = "Live stroke out of sync";

/// The stroke part of a live frame. With the recipe, `from` is the number of
/// points its line holds; without it, `points` are those added since `from`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveStroke {
    pub id: u64,
    pub mask_id: String,
    pub sub_mask_id: String,
    pub from: usize,
    #[serde(default)]
    pub points: Vec<Value>,
}

/// The recipe of the stroke being drawn, as the last frame left it.
#[derive(Default)]
pub struct LiveStrokeRecipe(Option<Held>);

struct Held {
    id: u64,
    sequence: u64,
    adjustments: Value,
}

impl LiveStrokeRecipe {
    /// The recipe a preview request renders. A request without a stroke
    /// forgets the held one; a recipe with a stroke is held for the points
    /// that follow; points alone are appended to the held stroke's line.
    pub fn resolve(
        &mut self,
        adjustments: Option<Value>,
        stroke: Option<LiveStroke>,
        sequence: Option<u64>,
    ) -> Result<Value, String> {
        let sequence = sequence.unwrap_or(0);
        match (adjustments, stroke) {
            (Some(adjustments), None) => {
                self.0 = None;
                Ok(adjustments)
            }
            (Some(adjustments), Some(stroke)) => {
                let newer = self.0.as_ref().is_none_or(|held| sequence > held.sequence);
                let holds = stroke_points(&adjustments, &stroke)
                    .is_some_and(|points| points.len() == stroke.from);
                if newer {
                    self.0 = holds.then(|| Held {
                        id: stroke.id,
                        sequence,
                        adjustments: adjustments.clone(),
                    });
                }
                Ok(adjustments)
            }
            (None, Some(stroke)) => {
                let held = self
                    .0
                    .as_mut()
                    .filter(|held| held.id == stroke.id && sequence > held.sequence)
                    .ok_or(LIVE_STROKE_RESYNC)?;
                let points = stroke_points_mut(&mut held.adjustments, &stroke)
                    .filter(|points| points.len() == stroke.from)
                    .ok_or(LIVE_STROKE_RESYNC)?;
                points.extend(stroke.points);
                held.sequence = sequence;
                Ok(held.adjustments.clone())
            }
            (None, None) => Err("No adjustments".to_string()),
        }
    }
}

/// The points of the stroke's line: the last line of its component.
fn stroke_points<'a>(adjustments: &'a Value, stroke: &LiveStroke) -> Option<&'a Vec<Value>> {
    adjustments
        .get("masks")?
        .as_array()?
        .iter()
        .find(|mask| mask.get("id").and_then(Value::as_str) == Some(&stroke.mask_id))?
        .get("subMasks")?
        .as_array()?
        .iter()
        .find(|sub| sub.get("id").and_then(Value::as_str) == Some(&stroke.sub_mask_id))?
        .get("parameters")?
        .get("lines")?
        .as_array()?
        .last()?
        .get("points")?
        .as_array()
}

fn stroke_points_mut<'a>(
    adjustments: &'a mut Value,
    stroke: &LiveStroke,
) -> Option<&'a mut Vec<Value>> {
    adjustments
        .get_mut("masks")?
        .as_array_mut()?
        .iter_mut()
        .find(|mask| mask.get("id").and_then(Value::as_str) == Some(&stroke.mask_id))?
        .get_mut("subMasks")?
        .as_array_mut()?
        .iter_mut()
        .find(|sub| sub.get("id").and_then(Value::as_str) == Some(&stroke.sub_mask_id))?
        .get_mut("parameters")?
        .get_mut("lines")?
        .as_array_mut()?
        .last_mut()?
        .get_mut("points")?
        .as_array_mut()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recipe(points: usize) -> Value {
        let points: Vec<Value> = (0..points).map(|i| json!({"x": i, "y": 0})).collect();
        json!({
            "exposure": 0.5,
            "masks": [
                {"id": "other", "subMasks": [{"id": "s", "parameters": {"lines": [{"points": []}]}}]},
                {"id": "layer", "subMasks": [
                    {"id": "radial", "parameters": {}},
                    {"id": "brush", "parameters": {"lines": [
                        {"points": [{"x": 9, "y": 9}]},
                        {"brushSize": 50, "points": points}
                    ]}}
                ]}
            ]
        })
    }

    fn stroke(id: u64, from: usize, points: std::ops::Range<usize>) -> LiveStroke {
        LiveStroke {
            id,
            mask_id: "layer".into(),
            sub_mask_id: "brush".into(),
            from,
            points: points.map(|i| json!({"x": i, "y": 0})).collect(),
        }
    }

    #[test]
    fn appended_points_give_the_whole_recipe() {
        let mut held = LiveStrokeRecipe::default();
        assert_eq!(
            held.resolve(Some(recipe(3)), Some(stroke(1, 3, 0..0)), Some(10)),
            Ok(recipe(3))
        );
        assert_eq!(
            held.resolve(None, Some(stroke(1, 3, 3..5)), Some(11)),
            Ok(recipe(5))
        );
        assert_eq!(
            held.resolve(None, Some(stroke(1, 5, 5..9)), Some(12)),
            Ok(recipe(9))
        );
    }

    #[test]
    fn points_that_do_not_continue_the_stroke_ask_for_the_recipe() {
        let mut held = LiveStrokeRecipe::default();
        // Nothing held yet.
        assert_eq!(
            held.resolve(None, Some(stroke(1, 0, 0..2)), Some(1)),
            Err(LIVE_STROKE_RESYNC.into())
        );
        held.resolve(Some(recipe(3)), Some(stroke(1, 3, 0..0)), Some(10))
            .unwrap();
        // A frame was overtaken: its successor starts past the held points.
        assert_eq!(
            held.resolve(None, Some(stroke(1, 5, 5..7)), Some(12)),
            Err(LIVE_STROKE_RESYNC.into())
        );
        // Another stroke, or a frame older than the held recipe.
        assert_eq!(
            held.resolve(None, Some(stroke(2, 3, 3..4)), Some(13)),
            Err(LIVE_STROKE_RESYNC.into())
        );
        assert_eq!(
            held.resolve(None, Some(stroke(1, 3, 3..4)), Some(9)),
            Err(LIVE_STROKE_RESYNC.into())
        );
        // The held stroke is unchanged by refused points.
        assert_eq!(
            held.resolve(None, Some(stroke(1, 3, 3..4)), Some(14)),
            Ok(recipe(4))
        );
    }

    #[test]
    fn a_recipe_without_the_stroke_forgets_it() {
        let mut held = LiveStrokeRecipe::default();
        held.resolve(Some(recipe(3)), Some(stroke(1, 3, 0..0)), Some(10))
            .unwrap();
        assert_eq!(held.resolve(Some(recipe(1)), None, Some(11)), Ok(recipe(1)));
        assert_eq!(
            held.resolve(None, Some(stroke(1, 3, 3..4)), Some(12)),
            Err(LIVE_STROKE_RESYNC.into())
        );
    }

    #[test]
    fn a_recipe_whose_line_differs_is_not_held() {
        let mut held = LiveStrokeRecipe::default();
        // The recipe's line holds 3 points, the frame says 4.
        assert_eq!(
            held.resolve(Some(recipe(3)), Some(stroke(1, 4, 0..0)), Some(10)),
            Ok(recipe(3))
        );
        assert_eq!(
            held.resolve(None, Some(stroke(1, 4, 4..5)), Some(11)),
            Err(LIVE_STROKE_RESYNC.into())
        );
        // An older recipe arriving late does not replace a newer one.
        held.resolve(Some(recipe(3)), Some(stroke(1, 3, 0..0)), Some(20))
            .unwrap();
        held.resolve(Some(recipe(2)), Some(stroke(1, 2, 0..0)), Some(15))
            .unwrap();
        assert_eq!(
            held.resolve(None, Some(stroke(1, 3, 3..4)), Some(21)),
            Ok(recipe(4))
        );
    }
}
