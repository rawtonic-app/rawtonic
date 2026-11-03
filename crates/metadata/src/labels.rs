//! Colour labels as other applications write them in `xmp:Label`.
//!
//! Lightroom writes the name of the label in its interface language (a
//! German Lightroom Classic 15.6 writes `Rot`, `Gelb`, `Grün`, `Blau`,
//! `Lila`) and matches the names it reads against its own label set only: an
//! English `Blue` shows as a white custom label there. Capture One writes the
//! English names, two more colours (`Orange`, `Pink`) and, by default, the
//! colour's number in `photoshop:Urgency` (Red 1, Green 2, Blue 3, Pink 4,
//! Purple 5, Orange 6, Yellow 7); for no tag it writes the label
//! `None` with urgency 0. It ignores an empty label and then reads the colour
//! back from the urgency, but takes `None` as no tag whatever the urgency
//! says. Observed on scratch sidecars (notes:
//! benchmark-results/xi-xmp-interop-2026-11-01/REPORT.md).

/// One of Rawtonic's colour labels (Lightroom's default set). A photo's
/// stored label is a string: one of these by [`ColorLabel::name`], empty for
/// none, or another application's name it keeps (Capture One's `Orange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ColorLabel {
    Red,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl ColorLabel {
    pub const ALL: [ColorLabel; 5] = [
        ColorLabel::Red,
        ColorLabel::Yellow,
        ColorLabel::Green,
        ColorLabel::Blue,
        ColorLabel::Purple,
    ];

    /// The stored name (`Red`, …).
    pub const fn name(self) -> &'static str {
        match self {
            ColorLabel::Red => "Red",
            ColorLabel::Yellow => "Yellow",
            ColorLabel::Green => "Green",
            ColorLabel::Blue => "Blue",
            ColorLabel::Purple => "Purple",
        }
    }

    /// The label a name in one of the common interface languages stands
    /// for (case and surrounding space ignored).
    pub fn from_name(name: &str) -> Option<Self> {
        let lower = name.trim().to_lowercase();
        let label = match lower.as_str() {
            "red" | "rot" | "rouge" | "rojo" | "rosso" | "vermelho" | "rood" | "czerwony"
            | "красный" | "赤" | "红色" | "紅色" | "빨간색" => ColorLabel::Red,
            "yellow" | "gelb" | "jaune" | "amarillo" | "giallo" | "amarelo" | "geel" | "żółty"
            | "желтый" | "黄" | "黄色" | "노란색" => ColorLabel::Yellow,
            "green" | "grün" | "vert" | "verde" | "groen" | "zielony" | "зеленый" | "緑"
            | "绿色" | "綠色" | "녹색" => ColorLabel::Green,
            "blue" | "blau" | "bleu" | "azul" | "blu" | "blauw" | "niebieski" | "синий" | "青"
            | "蓝色" | "藍色" | "파란색" => ColorLabel::Blue,
            "purple"
            | "violett"
            | "lila"
            | "violet"
            | "morado"
            | "púrpura"
            | "viola"
            | "roxo"
            | "paars"
            | "fioletowy"
            | "фиолетовый"
            | "紫"
            | "紫色"
            | "보라색" => ColorLabel::Purple,
            _ => return None,
        };
        Some(label)
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// Rawtonic's colour labels (Lightroom's default set), as stored.
pub const LABELS: [&str; 5] = [
    ColorLabel::Red.name(),
    ColorLabel::Yellow.name(),
    ColorLabel::Green.name(),
    ColorLabel::Blue.name(),
    ColorLabel::Purple.name(),
];

/// Lightroom's default label names per interface language, in the order of
/// [`LABELS`], with other names in that language that identify it. Only
/// languages whose names were observed are listed; a label in another
/// language is written in English.
const NAMES: [([&str; 5], &[&str]); 2] = [
    (["Red", "Yellow", "Green", "Blue", "Purple"], &[]),
    (["Rot", "Gelb", "Grün", "Blau", "Lila"], &["Violett"]),
];

/// The label a name stands for in one of the common interface languages
/// ([`LABELS`]), or `None` for a name that is not one of them.
pub fn canonical(name: &str) -> Option<&'static str> {
    ColorLabel::from_name(name).map(ColorLabel::name)
}

/// The label as Rawtonic stores it: one of [`LABELS`] for a name in any
/// listed language, nothing for Capture One's `None`, otherwise the name as
/// written (Capture One's `Orange`).
pub fn stored(name: &str) -> String {
    let name = name.trim();
    if name.eq_ignore_ascii_case("none") {
        return String::new();
    }
    canonical(name).map_or_else(|| name.to_string(), str::to_string)
}

/// Capture One's label for no colour tag.
pub const NONE: &str = "None";

/// The text to write for `label` into a sidecar that holds `previous`: the
/// sidecar's own text while it names the same colour, else the label's name
/// in the language of `previous`, so a label set in Rawtonic shows in the
/// colour it has in the application that labelled the photo before.
pub fn for_sidecar(label: &str, previous: &str) -> String {
    let Some(index) = ColorLabel::from_name(label).map(ColorLabel::index) else {
        return label.to_string();
    };
    if ColorLabel::from_name(previous).map(ColorLabel::index) == Some(index) {
        return previous.trim().to_string();
    }
    let previous = previous.trim();
    let same = |name: &&str| name.to_lowercase() == previous.to_lowercase();
    NAMES
        .iter()
        .find(|(names, others)| names.iter().any(same) || others.iter().any(same))
        .map_or(LABELS[index], |(names, _)| names[index])
        .to_string()
}

/// The editor state's tags carry the label as `color:<lower-case name>`.
pub const EDITOR_TAG_PREFIX: &str = "color:";

/// The editor tag of a stored label (not empty).
pub fn editor_tag(label: &str) -> String {
    format!("{EDITOR_TAG_PREFIX}{}", label.to_lowercase())
}

/// The stored label of an editor tag (its name with the first letter upper
/// case), or `None` for a tag that is not a label's.
pub fn from_editor_tag(tag: &str) -> Option<String> {
    let name = tag.strip_prefix(EDITOR_TAG_PREFIX)?;
    let mut chars = name.chars();
    Some(
        chars
            .next()
            .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
            .unwrap_or_default(),
    )
}

/// Whether an editor tag is a label's.
pub fn is_editor_tag(tag: &str) -> bool {
    tag.starts_with(EDITOR_TAG_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_in_several_languages() {
        assert_eq!(canonical("Rot"), Some("Red"));
        assert_eq!(canonical(" violet "), Some("Purple"));
        assert_eq!(canonical("Orange"), None);
        assert_eq!(stored("Lila"), "Purple");
        assert_eq!(stored("Pink"), "Pink");
        assert_eq!(stored("None"), "");
    }

    #[test]
    fn labels_have_their_stored_names_in_order() {
        assert_eq!(LABELS, ["Red", "Yellow", "Green", "Blue", "Purple"]);
        for (label, name) in ColorLabel::ALL.into_iter().zip(LABELS) {
            assert_eq!(label.name(), name);
            assert_eq!(ColorLabel::from_name(name), Some(label));
            assert_eq!(LABELS[label.index()], name);
        }
    }

    #[test]
    fn editor_tags_round_trip_the_stored_label() {
        assert_eq!(editor_tag("Red"), "color:red");
        assert_eq!(editor_tag("Orange"), "color:orange");
        assert_eq!(from_editor_tag("color:red").as_deref(), Some("Red"));
        assert_eq!(from_editor_tag("color:grün").as_deref(), Some("Grün"));
        assert_eq!(from_editor_tag("color:").as_deref(), Some(""));
        assert_eq!(from_editor_tag("user:red"), None);
        for label in LABELS {
            assert_eq!(from_editor_tag(&editor_tag(label)).as_deref(), Some(label));
        }
        assert!(is_editor_tag("color:blue") && !is_editor_tag("blue"));
    }

    #[test]
    fn a_sidecar_keeps_its_language_and_its_own_text() {
        assert_eq!(for_sidecar("Red", "Rot"), "Rot");
        assert_eq!(for_sidecar("Blue", "Rot"), "Blau");
        assert_eq!(for_sidecar("Purple", "gelb"), "Lila");
        assert_eq!(for_sidecar("Blue", "Red"), "Blue");
        assert_eq!(for_sidecar("Blue", ""), "Blue");
        assert_eq!(for_sidecar("Blue", "Rouge"), "Blue");
        assert_eq!(for_sidecar("Orange", "Rot"), "Orange");
        assert_eq!(for_sidecar("Red", "Violett"), "Rot");
    }
}
