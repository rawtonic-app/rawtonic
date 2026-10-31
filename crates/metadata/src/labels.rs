//! Colour labels as other applications write them in `xmp:Label`.
//!
//! Lightroom writes the name of the label in its interface language (a
//! German Lightroom Classic 15.6 writes `Rot`, `Gelb`, `Grün`, `Blau`,
//! `Lila`) and matches the names it reads against its own label set only: an
//! English `Blue` shows as a white custom label there. Capture One writes the
//! English names, two more colours (`Orange`, `Pink`) and, by default, the
//! colour's number in `photoshop:Urgency`, which it reads back when the label
//! is empty. Observed on scratch sidecars (notes:
//! benchmark-results/xi-xmp-interop-2026-11-01/REPORT.md).

/// Rawtonic's colour labels (Lightroom's default set), as stored.
pub const LABELS: [&str; 5] = ["Red", "Yellow", "Green", "Blue", "Purple"];

/// Lightroom's default label names per interface language, in the order of
/// [`LABELS`], with other names in that language that identify it. Only
/// languages whose names were observed are listed; a label in another
/// language is written in English.
const NAMES: [([&str; 5], &[&str]); 2] = [
    (["Red", "Yellow", "Green", "Blue", "Purple"], &[]),
    (["Rot", "Gelb", "Grün", "Blau", "Lila"], &["Violett"]),
];

/// Capture One's colour tags and the `photoshop:Urgency` it writes for each.
const URGENCY: [(&str, &str); 7] = [
    ("Red", "1"),
    ("Green", "2"),
    ("Blue", "3"),
    ("Pink", "4"),
    ("Purple", "5"),
    ("Orange", "6"),
    ("Yellow", "7"),
];

/// The label a name stands for in one of the common interface languages
/// ([`LABELS`]), or `None` for a name that is not one of them.
pub fn canonical(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_lowercase();
    let label = match lower.as_str() {
        "red" | "rot" | "rouge" | "rojo" | "rosso" | "vermelho" | "rood" | "czerwony"
        | "красный" | "赤" | "红色" | "紅色" | "빨간색" => "Red",
        "yellow" | "gelb" | "jaune" | "amarillo" | "giallo" | "amarelo" | "geel" | "żółty"
        | "желтый" | "黄" | "黄色" | "노란색" => "Yellow",
        "green" | "grün" | "vert" | "verde" | "groen" | "zielony" | "зеленый" | "緑" | "绿色"
        | "綠色" | "녹색" => "Green",
        "blue" | "blau" | "bleu" | "azul" | "blu" | "blauw" | "niebieski" | "синий" | "青"
        | "蓝色" | "藍色" | "파란색" => "Blue",
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
        | "보라색" => "Purple",
        _ => return None,
    };
    Some(label)
}

/// The label as Rawtonic stores it: one of [`LABELS`] for a name in any
/// listed language, otherwise the name as written (Capture One's `Orange`).
pub fn stored(name: &str) -> String {
    canonical(name).map_or_else(|| name.trim().to_string(), str::to_string)
}

/// The text to write for `label` into a sidecar that holds `previous`: the
/// sidecar's own text while it names the same colour, else the label's name
/// in the language of `previous`, so a label set in Rawtonic shows in the
/// colour it has in the application that labelled the photo before.
pub fn for_sidecar(label: &str, previous: &str) -> String {
    let Some(index) = canonical(label).and_then(|l| LABELS.iter().position(|x| *x == l)) else {
        return label.to_string();
    };
    if canonical(previous) == Some(LABELS[index]) {
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

/// Capture One's `photoshop:Urgency` for a label, if it has one.
pub fn urgency(label: &str) -> Option<&'static str> {
    let label = stored(label);
    URGENCY
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&label))
        .map(|(_, code)| *code)
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

    #[test]
    fn capture_one_urgency_of_each_colour() {
        assert_eq!(urgency("Rot"), Some("1"));
        assert_eq!(urgency("Orange"), Some("6"));
        assert_eq!(urgency(""), None);
    }
}
