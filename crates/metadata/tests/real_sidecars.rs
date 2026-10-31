//! Sidecars written by Lightroom Classic 15.6 (German interface) and Capture
//! One 16.6.6 for CC0 scratch photos, scrubbed (no profile or look data, no
//! serial numbers or personal names): Rawtonic reads what they hold and,
//! writing its changes, keeps what they wrote and it does not model.
//! notes: benchmark-results/xi-xmp-interop-2026-11-01/REPORT.md
use rawtonic_metadata::{Metadata, merge_xmp, read_xmp};

const LIGHTROOM: &str = include_str!("fixtures/lightroom-classic-15.6-de.xmp");
const C1_KEYWORDS: &str = include_str!("fixtures/capture-one-16.6-keywords.xmp");
const C1_IPTC: &str = include_str!("fixtures/capture-one-16.6-iptc.xmp");

fn read(text: &str) -> Metadata {
    read_xmp(text).unwrap().0
}

/// The source text of every element and attribute in `text` that holds
/// none of `owned`, so a test can check they come back byte for byte.
fn foreign_parts(text: &str, owned: &[&str]) -> Vec<String> {
    let doc = roxmltree::Document::parse(text).unwrap();
    let description = doc
        .descendants()
        .find(|n| n.tag_name().name() == "Description")
        .unwrap();
    let is_owned = |qname: &str| owned.contains(&qname);
    let mut parts: Vec<String> = description
        .attributes()
        .map(|a| text[a.range()].to_string())
        .filter(|a| !is_owned(a.split('=').next().unwrap()))
        .collect();
    for child in description.children().filter(|n| n.is_element()) {
        let source = &text[child.range()];
        let qname = source[1..]
            .split(|c: char| c.is_whitespace() || c == '>')
            .next()
            .unwrap();
        if !is_owned(qname) {
            parts.push(source.to_string());
        }
    }
    parts
}

/// What Rawtonic models: rating, label, flags, keywords, Dublin Core and the
/// IPTC Core fields it edits (Lightroom's and Capture One's prefixes).
const OWNED: &[&str] = &[
    "xmp:Rating",
    "xmp:Label",
    "photoshop:LabelColor",
    "xmpDM:pick",
    "xmpDM:good",
    "dc:title",
    "dc:description",
    "dc:creator",
    "dc:rights",
    "dc:subject",
    "lr:hierarchicalSubject",
    "lightroom:hierarchicalSubject",
    "photoshop:Headline",
    "photoshop:City",
    "photoshop:State",
    "photoshop:Country",
    "photoshop:TransmissionReference",
    "photoshop:Instructions",
    "photoshop:Credit",
    "photoshop:Source",
    "photoshop:AuthorsPosition",
    "photoshop:Urgency",
    "Iptc4xmpCore:Location",
    "Iptc4xmpCore:CountryCode",
    "Iptc4xmpCore:CreatorContactInfo",
    "xmpRights:UsageTerms",
];

#[test]
fn lightroom_labels_flags_and_keywords_are_read_as_lightroom_shows_them() {
    let meta = read(LIGHTROOM);
    assert_eq!(meta.rating, 1);
    // German "Gelb" is the yellow label, not a label named "Gelb".
    assert_eq!(meta.label, "Yellow");
    // Lightroom's reject (xmpDM:pick -1).
    assert_eq!(meta.flag, -1);
    // The keyword paths only, not their levels again as keywords of their own.
    assert_eq!(
        meta.keywords,
        ["Animals|Bird", "Places|Europe|Berlin", "Street"]
    );
    assert_eq!(meta.title, "Title 2 – Spree bank");
    assert_eq!(meta.creator, "Test Creator");
    assert_eq!(meta.iptc["headline"], "Headline 2");
    assert_eq!(meta.iptc["creatorEmail"], "creator@example.invalid");
    assert_eq!(meta.iptc["usageTerms"], "Editorial use only");
}

#[test]
fn a_pick_set_in_lightroom_wins_over_the_flag_rawtonic_wrote_before() {
    let mut meta = read(LIGHTROOM);
    meta.flag = 1;
    let ours = merge_xmp(Some(LIGHTROOM), &meta, None).unwrap();
    // Lightroom rewrites its pick, leaving Rawtonic's own property as it was.
    let theirs = ours
        .replace("xmpDM:pick=\"1\"", "xmpDM:pick=\"0\"")
        .replace(" xmpDM:good=\"true\"", "");
    assert_eq!(read(&theirs).flag, 0);
}

#[test]
fn rawtonic_writes_lightroom_labels_flags_and_keywords_lightroom_reads() {
    let mut meta = read(LIGHTROOM);
    meta.label = "Blue".into();
    meta.flag = 1;
    meta.keywords.push("Places|Europe|Paris".into());
    let written = merge_xmp(Some(LIGHTROOM), &meta, None).unwrap();
    // Lightroom matches label names against its own (German) label set.
    assert!(written.contains("xmp:Label=\"Blau\""), "{written}");
    assert!(written.contains("photoshop:LabelColor=\"blue\""));
    assert!(written.contains("xmpDM:pick=\"1\" xmpDM:good=\"true\""));
    assert!(!written.contains("LabelColor=\"yellow\""));
    // dc:subject holds terms, the hierarchy stays in hierarchicalSubject.
    let doc = roxmltree::Document::parse(&written).unwrap();
    let items = |name: &str| -> Vec<String> {
        doc.descendants()
            .filter(|n| n.tag_name().name() == name)
            .flat_map(|n| n.descendants().filter(|l| l.tag_name().name() == "li"))
            .map(|l| l.text().unwrap_or("").to_string())
            .collect()
    };
    let subject = items("subject");
    assert!(subject.iter().all(|t| !t.contains('|')), "{subject:?}");
    assert!(subject.contains(&"Paris".to_string()));
    assert_eq!(
        items("hierarchicalSubject"),
        [
            "Animals|Bird",
            "Places|Europe|Berlin",
            "Street",
            "Places|Europe|Paris"
        ]
    );
    let back = read(&written);
    assert_eq!(back.label, "Blue");
    assert_eq!(back.flag, 1);
    assert_eq!(back.keywords, meta.keywords);
    // Develop settings, GPS, IPTC Extension and history come back as written.
    for part in foreign_parts(LIGHTROOM, OWNED) {
        assert!(written.contains(&part), "lost: {part}");
    }
}

#[test]
fn an_unchanged_label_keeps_the_text_lightroom_wrote() {
    let meta = read(LIGHTROOM);
    let written = merge_xmp(Some(LIGHTROOM), &meta, None).unwrap();
    assert!(written.contains("xmp:Label=\"Gelb\""));
    assert!(written.contains("xmpDM:pick=\"-1\" xmpDM:good=\"false\""));
}

#[test]
fn capture_one_keywords_are_its_entries_without_the_flat_terms() {
    let meta = read(C1_KEYWORDS);
    assert_eq!(meta.rating, 3);
    assert_eq!(meta.label, "Red");
    // Every entry Capture One assigned (also its ancestors and the levels it
    // took over as keywords of their own); `dc:subject` adds nothing more.
    assert_eq!(
        meta.keywords,
        [
            "Places|Europe|Berlin",
            "Street",
            "Europe",
            "Places",
            "Travel",
            "Travel|Italy",
            "Travel|Italy|Lugano",
            "Tea & Cake"
        ]
    );
    let written = merge_xmp(Some(C1_KEYWORDS), &meta, None).unwrap();
    assert_eq!(read(&written).keywords, meta.keywords);
    for part in foreign_parts(C1_KEYWORDS, OWNED) {
        assert!(written.contains(&part), "lost: {part}");
    }
}

#[test]
fn a_root_keyword_named_like_a_level_of_a_path_is_kept() {
    // `Berlin` assigned on its own beside `Places|Germany|Berlin`.
    let packet = LIGHTROOM
        .replace(
            "<rdf:li>Animals|Bird</rdf:li>",
            "<rdf:li>Berlin</rdf:li>\n     <rdf:li>Places|Germany|Berlin</rdf:li>",
        )
        .replace("<rdf:li>Animals</rdf:li>", "<rdf:li>Germany</rdf:li>")
        .replace("<rdf:li>Bird</rdf:li>", "<rdf:li>Places</rdf:li>");
    let meta = read(&packet);
    assert_eq!(
        meta.keywords,
        [
            "Berlin",
            "Places|Germany|Berlin",
            "Places|Europe|Berlin",
            "Street"
        ]
    );
    let written = merge_xmp(Some(&packet), &meta, None).unwrap();
    assert_eq!(read(&written).keywords, meta.keywords);
}

#[test]
fn capture_one_creators_and_iptc_survive_a_rewrite() {
    let meta = read(C1_IPTC);
    // Capture One adds the camera's artist to the creator set by hand.
    assert_eq!(meta.creator, "Camera Artist; Test Creator");
    assert_eq!(meta.label, "Green");
    assert_eq!(meta.iptc["creatorCity"], "Exampletown");
    assert_eq!(meta.iptc["location"], "Riverbank");
    let written = merge_xmp(Some(C1_IPTC), &meta, None).unwrap();
    assert!(written.contains("<rdf:li>Camera Artist</rdf:li><rdf:li>Test Creator</rdf:li>"));
    assert_eq!(read(&written), meta);
    for part in foreign_parts(C1_IPTC, OWNED) {
        assert!(written.contains(&part), "lost: {part}");
    }
}

#[test]
fn capture_one_none_is_no_label() {
    // What Capture One writes when its colour tag is set to none.
    let none = C1_IPTC
        .replace(
            "<xmp:Label>Green</xmp:Label>",
            "<xmp:Label>None</xmp:Label>",
        )
        .replace(
            "<photoshop:Urgency>2</photoshop:Urgency>",
            "<photoshop:Urgency>0</photoshop:Urgency>",
        );
    let meta = read(&none);
    assert_eq!(meta.label, "");
    // Writing it back leaves Capture One's own urgency as it was.
    let written = merge_xmp(Some(&none), &meta, None).unwrap();
    assert!(written.contains("<photoshop:Urgency>0</photoshop:Urgency>"));
    assert!(!written.contains("photoshop:Urgency=\""));
}

#[test]
fn a_label_cleared_in_rawtonic_stays_cleared_in_capture_one() {
    // Capture One takes an empty label as unchanged and brings its colour
    // back from the urgency; it takes its own `None` as no tag.
    let mut meta = read(C1_IPTC);
    meta.label.clear();
    let cleared = merge_xmp(Some(C1_IPTC), &meta, None).unwrap();
    assert!(cleared.contains("xmp:Label=\"None\""), "{cleared}");
    assert_eq!(read(&cleared).label, "");
    // Saved again (another change before Capture One reads it): still none.
    let mut again = read(&cleared);
    again.rating = 4;
    let saved = merge_xmp(Some(&cleared), &again, None).unwrap();
    assert!(saved.contains("xmp:Label=\"None\""), "{saved}");
    assert!(saved.contains("<photoshop:Urgency>2</photoshop:Urgency>"));
    // A label set again is written as usual.
    meta.label = "Blue".into();
    let blue = merge_xmp(Some(&saved), &meta, None).unwrap();
    assert!(blue.contains("xmp:Label=\"Blue\""));
}

#[test]
fn a_label_changed_then_cleared_stays_cleared_in_capture_one() {
    // Green → Blue → none, each saved before Capture One rewrites the urgency.
    let mut meta = read(C1_IPTC);
    meta.label = "Blue".into();
    let blue = merge_xmp(Some(C1_IPTC), &meta, None).unwrap();
    meta.label.clear();
    let cleared = merge_xmp(Some(&blue), &meta, None).unwrap();
    assert!(cleared.contains("xmp:Label=\"None\""), "{cleared}");
    assert!(cleared.contains("<photoshop:Urgency>2</photoshop:Urgency>"));
}

#[test]
fn the_urgency_is_never_changed() {
    // It may be the photographer's own, even when it matches the label's
    // number: only Capture One rewrites it.
    let mut meta = read(C1_IPTC);
    for label in ["", "Blue", "Red"] {
        meta.label = label.into();
        let written = merge_xmp(Some(C1_IPTC), &meta, None).unwrap();
        assert!(written.contains("<photoshop:Urgency>2</photoshop:Urgency>"));
        assert!(!written.contains("photoshop:Urgency=\""));
    }
    // A photographer's own urgency stays through label changes too.
    let own = C1_IPTC.replace(
        "<photoshop:Urgency>2</photoshop:Urgency>",
        "<photoshop:Urgency>8</photoshop:Urgency>",
    );
    for label in ["Blue", ""] {
        meta.label = label.into();
        let written = merge_xmp(Some(&own), &meta, None).unwrap();
        assert!(written.contains("<photoshop:Urgency>8</photoshop:Urgency>"));
    }
}

#[test]
fn lightroom_labels_are_cleared_to_no_label_even_beside_an_urgency() {
    // Lightroom shows `None` as a label of its own; its sidecars carry
    // `photoshop:LabelColor` beside the label.
    let mut meta = read(LIGHTROOM);
    meta.label.clear();
    let plain = merge_xmp(Some(LIGHTROOM), &meta, None).unwrap();
    assert!(plain.contains("xmp:Label=\"\""));
    let with_urgency = LIGHTROOM.replace(
        "   xmp:Label=\"Gelb\"\n",
        "   xmp:Label=\"Gelb\"\n   photoshop:Urgency=\"8\"\n",
    );
    assert!(with_urgency.contains("photoshop:Urgency=\"8\""));
    let written = merge_xmp(Some(&with_urgency), &meta, None).unwrap();
    assert!(written.contains("xmp:Label=\"\""), "{written}");
    assert!(written.contains("photoshop:Urgency=\"8\""));
}

#[test]
fn a_photo_without_a_label_never_gets_one() {
    // Only a rating changes on an unlabelled photo with an urgency.
    let unlabelled = C1_IPTC
        .replace("   <xmp:Label>Green</xmp:Label>\n", "")
        .replace(
            "<photoshop:Urgency>2</photoshop:Urgency>",
            "<photoshop:Urgency>8</photoshop:Urgency>",
        );
    let mut meta = read(&unlabelled);
    assert_eq!(meta.label, "");
    meta.rating = 5;
    let written = merge_xmp(Some(&unlabelled), &meta, None).unwrap();
    assert!(written.contains("xmp:Label=\"\""), "{written}");
    assert!(!written.contains("LabelColor"));
}

#[test]
fn capture_one_extra_colours_keep_their_exact_property_on_unrelated_edits() {
    for label in ["Orange", "Pink"] {
        let packet = C1_KEYWORDS.replace(
            "<xmp:Label>Red</xmp:Label>",
            &format!("<xmp:Label>{label}</xmp:Label>"),
        );
        let mut metadata = read(&packet);
        assert_eq!(metadata.label, label);
        metadata.rating = 4;
        let written = merge_xmp(Some(&packet), &metadata, None).unwrap();
        assert!(written.contains(&format!("<xmp:Label>{label}</xmp:Label>")));
        assert_eq!(read(&written).label, label);
    }
}
