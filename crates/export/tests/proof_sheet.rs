//! Proof sheets written and read back: a valid multi-page PDF holding the
//! rendered photos unchanged, the captions as text, and the page JPEGs.
use rawtonic_export::proof_sheet::{CellImage, PageValues, ProofSheet, write_sheet};
use rawtonic_export::text_watermark::Typeface;

fn jpeg(w: u32, h: u32, rgb: [u8; 3], gray: bool) -> Vec<u8> {
    let image = if gray {
        image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(w, h, image::Luma([rgb[0]])))
    } else {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb(rgb)))
    };
    let mut bytes = Vec::new();
    image
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    bytes
}

fn sheet() -> ProofSheet {
    serde_json::from_value(serde_json::json!({
        "pageWidthMm": 210.0, "pageHeightMm": 297.0,
        "marginsMm": [12.0, 12.0, 12.0, 12.0],
        "columns": 4, "rows": 5, "gapMm": 4.0,
        "caption": "{seq} {orig}",
        "header": "{job} · {date}",
        "footer": "{count} photos",
        "pageNumbers": true,
        "ppi": 100,
        "pdf": true, "jpeg": true,
    }))
    .unwrap()
}

#[test]
fn a_proof_sheet_reads_back_as_the_pages_photos_and_text_it_holds() {
    if Typeface::load(None).is_none() {
        // No fonts on this machine (minimal CI images): no text to set.
        return;
    }
    let sheet = sheet();
    let layout = sheet.layout(23).unwrap();
    let (bw, bh) = layout.box_px;
    let mut cells: Vec<Option<CellImage>> = (0..23)
        .map(|i| {
            // Landscape and portrait photos fitted into the box.
            let (w, h) = if i % 2 == 0 {
                (bw, bw * 2 / 3)
            } else {
                (bh * 2 / 3, bh)
            };
            Some(CellImage {
                jpeg: jpeg(w, h.min(bh), [200, (i * 10) as u8, 40], i == 3),
            })
        })
        .collect();
    // A photo that failed leaves its cell empty.
    cells[5] = None;
    let captions: Vec<String> = (0..23)
        .map(|i| format!("{:02} DSC_{:04}.NEF", i + 1, i + 100))
        .collect();
    let mut captions = captions;
    captions[7] = "08 写真.jpg".into();
    let values = PageValues {
        job: "Smith wedding".into(),
        date: "2026-11-02".into(),
        count: 23,
    };
    let written = write_sheet(&sheet, &cells, &captions, &values, true).unwrap();
    let pdf = written.pdf.unwrap();
    // For a look by eye: RAWTONIC_PROOF_SHEET_DUMP=folder.
    if let Some(dir) = std::env::var_os("RAWTONIC_PROOF_SHEET_DUMP") {
        std::fs::write(std::path::Path::new(&dir).join("proof.pdf"), &pdf).unwrap();
    }

    let doc = lopdf::Document::load_mem(&pdf).expect("a valid PDF");
    let pages = doc.get_pages();
    assert_eq!(pages.len(), 2);
    let images_on = |page: u32| doc.get_page_images(pages[&page]).unwrap();
    assert_eq!(images_on(1).len(), 19, "20 cells less the failed one");
    assert_eq!(images_on(2).len(), 3);
    // The export's JPEGs go in unchanged.
    let first = images_on(1);
    for image in &first {
        assert_eq!(
            image.filters.as_deref(),
            Some(&["DCTDecode".to_string()][..])
        );
        assert!(cells.iter().flatten().any(|c| c.jpeg == image.content));
    }
    assert!(
        first
            .iter()
            .any(|i| i.color_space.as_deref() == Some("DeviceGray"))
    );
    assert!(
        first
            .iter()
            .any(|i| i.color_space.as_deref() == Some("ICCBased"))
    );
    let text = doc.extract_text(&[1]).unwrap();
    for expected in [
        "Smith wedding",
        "2026-11-02",
        "01 DSC_0100.NEF",
        "20 DSC_0119.NEF",
        "23 photos",
        "1 / 2",
    ] {
        assert!(
            text.contains(expected),
            "{expected:?} missing from {text:?}"
        );
    }
    let text = doc.extract_text(&[2]).unwrap();
    assert!(
        text.contains("23 DSC_0122.NEF") && text.contains("2 / 2"),
        "{text:?}"
    );
    // AI-generated areas in a photo: the sheet carries the marking.
    let marked = String::from_utf8_lossy(&pdf);
    assert!(marked.contains(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI));

    // One JPEG per page at the sheet's resolution, photos where the PDF has them.
    assert_eq!(written.jpegs.len(), 2);
    let page = image::load_from_memory(&written.jpegs[0])
        .unwrap()
        .to_rgb8();
    let px = |mm: f64| (mm / 25.4 * 100.0).round() as u32;
    assert_eq!(page.dimensions(), (px(210.0), px(297.0)));
    let cell = &layout.pages[0].cells[0].photo_box;
    let centre = page.get_pixel(px(cell.x + cell.w / 2.0), px(cell.y + cell.h / 2.0));
    assert!(centre[0] > 180 && centre[2] < 70, "{centre:?}");
    // The failed photo's cell stays paper white.
    let empty = &layout.pages[0].cells[5].photo_box;
    let paper = page.get_pixel(px(empty.x + empty.w / 2.0), px(empty.y + empty.h / 2.0));
    assert!(paper.0.iter().all(|c| *c > 245), "{paper:?}");
    assert!(
        String::from_utf8_lossy(&written.jpegs[0])
            .contains(rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI)
    );
}

#[test]
fn without_generated_photos_the_sheet_carries_no_marking() {
    if Typeface::load(None).is_none() {
        return;
    }
    let mut sheet = sheet();
    sheet.jpeg = false;
    let cells = vec![Some(CellImage {
        jpeg: jpeg(100, 80, [10, 20, 30], false),
    })];
    let written =
        write_sheet(&sheet, &cells, &["1".into()], &PageValues::default(), false).unwrap();
    assert!(written.jpegs.is_empty());
    let pdf = written.pdf.unwrap();
    assert!(!String::from_utf8_lossy(&pdf).contains("DigitalSourceType"));
    assert_eq!(
        lopdf::Document::load_mem(&pdf).unwrap().get_pages().len(),
        1
    );
}
