//! Proof sheets (contact sheets) for client proofing: numbered photos in a
//! grid on printed pages, with a caption under each photo from the naming
//! tokens, a header and footer, written as one multi-page PDF and/or one
//! JPEG per page.
//!
//! The photos themselves are rendered by the export (each at its cell's
//! pixel size, with output sharpening and the watermark of the export); this
//! module lays out the pages, places those rendered JPEGs unchanged in the
//! PDF (DCTDecode, no second compression) and writes the text as vector
//! outlines of an installed font whose licence allows embedding, with an
//! invisible text layer so names can be selected, searched and copied.
//! The PDF writer is our own (PDF 1.7, a handful of object types).
use crate::text_watermark::{PathOp, Typeface};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

pub const MM_PER_INCH: f64 = 25.4;
const PT_PER_MM: f64 = 72.0 / MM_PER_INCH;

/// What the user set in the Proof Sheet dialog. Lengths in millimetres,
/// text sizes in points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofSheet {
    pub page_width_mm: f64,
    pub page_height_mm: f64,
    /// Top, right, bottom, left.
    pub margins_mm: [f64; 4],
    pub columns: u32,
    pub rows: u32,
    /// Space between cells.
    pub gap_mm: f64,
    /// Caption under each photo in the naming tokens (`rawtonic_naming`);
    /// none or empty for no caption.
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default = "default_caption_pt")]
    pub caption_pt: f64,
    /// Header and footer text: `{job}`, `{date}`, `{count}`, `{page}`,
    /// `{pages}` (`page_text`).
    #[serde(default)]
    pub header: Option<String>,
    #[serde(default)]
    pub footer: Option<String>,
    /// Page numbers at the right of the footer line.
    #[serde(default)]
    pub page_numbers: bool,
    #[serde(default = "default_text_pt")]
    pub text_pt: f64,
    /// Installed font family; none for the system's sans-serif.
    #[serde(default)]
    pub font_family: Option<String>,
    /// Crop (cut) marks at the corners of every cell, in the gaps and margins
    /// outside the cells; off unless asked for.
    #[serde(default)]
    pub crop_marks: bool,
    #[serde(default = "default_ppi")]
    pub ppi: u32,
    #[serde(default = "yes")]
    pub pdf: bool,
    #[serde(default)]
    pub jpeg: bool,
    #[serde(default = "default_quality")]
    pub jpeg_quality: u8,
}

fn default_caption_pt() -> f64 {
    8.0
}
fn default_text_pt() -> f64 {
    10.0
}
fn default_ppi() -> u32 {
    300
}
fn default_quality() -> u8 {
    92
}
fn yes() -> bool {
    true
}

/// A rectangle in millimetres, origin at the top left of the page, y down.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cell {
    /// Which photo of the sheet (0-based, in the sheet's order).
    pub photo: usize,
    pub cell: Rect,
    /// Where the photo is fitted (centred) inside the cell.
    pub photo_box: Rect,
    /// The caption's baseline, when there is a caption.
    pub caption_baseline: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    pub cells: Vec<Cell>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    pub page_width_mm: f64,
    pub page_height_mm: f64,
    pub pages: Vec<Page>,
    /// The photo box in output pixels at the sheet's ppi: every photo is
    /// rendered to fit inside it.
    pub box_px: (u32, u32),
    pub header_baseline: Option<f64>,
    pub footer_baseline: Option<f64>,
}

/// Height of a line of text of `pt` points, in millimetres.
fn line_mm(pt: f64) -> f64 {
    pt * 1.4 / PT_PER_MM
}

fn has_text(text: &Option<String>) -> bool {
    text.as_deref().is_some_and(|t| !t.trim().is_empty())
}

impl ProofSheet {
    pub fn photos_per_page(&self) -> usize {
        (self.columns * self.rows) as usize
    }

    /// The pages for `count` photos, or why this layout cannot be printed.
    pub fn layout(&self, count: usize) -> Result<Layout, String> {
        let finite = [
            self.page_width_mm,
            self.page_height_mm,
            self.gap_mm,
            self.caption_pt,
            self.text_pt,
        ]
        .iter()
        .chain(&self.margins_mm)
        .all(|v| v.is_finite());
        if !finite || self.margins_mm.iter().any(|m| *m < 0.0) || self.gap_mm < 0.0 {
            return Err("The page settings are not valid.".into());
        }
        if !(20.0..=2000.0).contains(&self.page_width_mm)
            || !(20.0..=2000.0).contains(&self.page_height_mm)
        {
            return Err("The page must be 20 to 2000 mm on each side.".into());
        }
        if !(1..=20).contains(&self.columns) || !(1..=30).contains(&self.rows) {
            return Err("Use 1 to 20 columns and 1 to 30 rows.".into());
        }
        if !(72..=1200).contains(&self.ppi) {
            return Err("The resolution must be 72 to 1200 ppi.".into());
        }
        if !(4.0..=72.0).contains(&self.caption_pt) || !(4.0..=72.0).contains(&self.text_pt) {
            return Err("Text sizes must be 4 to 72 pt.".into());
        }
        if !self.pdf && !self.jpeg {
            return Err("Choose PDF, JPEG or both.".into());
        }
        let [top, right, bottom, left] = self.margins_mm;
        let (pw, ph) = (self.page_width_mm, self.page_height_mm);
        let text_line = line_mm(self.text_pt);
        let header = has_text(&self.header);
        let footer = has_text(&self.footer) || self.page_numbers;
        let grid_top = top + if header { text_line * 1.5 } else { 0.0 };
        let grid_bottom = ph - bottom - if footer { text_line * 1.5 } else { 0.0 };
        let (grid_w, grid_h) = (pw - left - right, grid_bottom - grid_top);
        let (cols, rows) = (self.columns as f64, self.rows as f64);
        let cell_w = (grid_w - self.gap_mm * (cols - 1.0)) / cols;
        let cell_h = (grid_h - self.gap_mm * (rows - 1.0)) / rows;
        let caption_band = if has_text(&self.caption) {
            line_mm(self.caption_pt) * 1.2
        } else {
            0.0
        };
        let photo_h = cell_h - caption_band;
        if cell_w < 5.0 || photo_h < 5.0 {
            return Err(
                "The cells are too small: use fewer columns or rows, or smaller margins.".into(),
            );
        }
        let px = |mm: f64| (mm / MM_PER_INCH * self.ppi as f64).floor().max(1.0) as u32;
        let box_px = (px(cell_w), px(photo_h));
        if box_px.0 as u64 * box_px.1 as u64 > 200_000_000 {
            return Err("The cells are too large at this resolution.".into());
        }
        let per_page = self.photos_per_page();
        let page_count = count.div_ceil(per_page).max(1);
        let ascent = self.caption_pt * 0.8 / PT_PER_MM;
        let pages = (0..page_count)
            .map(|page| Page {
                cells: (0..per_page)
                    .map(|slot| page * per_page + slot)
                    .take_while(|&photo| photo < count)
                    .map(|photo| {
                        let slot = photo % per_page;
                        let (col, row) = (
                            (slot % self.columns as usize) as f64,
                            (slot / self.columns as usize) as f64,
                        );
                        let cell = Rect {
                            x: left + col * (cell_w + self.gap_mm),
                            y: grid_top + row * (cell_h + self.gap_mm),
                            w: cell_w,
                            h: cell_h,
                        };
                        Cell {
                            photo,
                            cell,
                            photo_box: Rect { h: photo_h, ..cell },
                            caption_baseline: (caption_band > 0.0)
                                .then_some(cell.y + photo_h + caption_band * 0.2 + ascent),
                        }
                    })
                    .collect(),
            })
            .collect();
        let text_ascent = self.text_pt * 0.8 / PT_PER_MM;
        Ok(Layout {
            page_width_mm: pw,
            page_height_mm: ph,
            pages,
            box_px,
            header_baseline: header.then_some(top + text_ascent),
            footer_baseline: footer.then_some(ph - bottom - text_line * 0.25),
        })
    }
}

/// What header and footer text is filled with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageValues {
    pub job: String,
    pub date: String,
    pub count: usize,
}

/// `template` with `{job}`, `{date}`, `{count}`, `{page}` and `{pages}`
/// filled; other braces stay as written.
pub fn page_text(template: &str, values: &PageValues, page: usize, pages: usize) -> String {
    template
        .replace("{job}", &values.job)
        .replace("{date}", &values.date)
        .replace("{count}", &values.count.to_string())
        .replace("{pages}", &pages.to_string())
        .replace("{page}", &page.to_string())
}

/// One rendered photo: the export's JPEG of it, fitted to the photo box.
#[derive(Debug, Clone)]
pub struct CellImage {
    pub jpeg: Vec<u8>,
}

/// Size and colour components of a baseline or progressive JPEG.
pub fn jpeg_info(jpeg: &[u8]) -> Option<(u32, u32, u8)> {
    if jpeg.get(..2) != Some(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    while at + 4 <= jpeg.len() {
        if jpeg[at] != 0xFF {
            return None;
        }
        let marker = jpeg[at + 1];
        if marker == 0xFF {
            at += 1;
            continue;
        }
        let length = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        let start_of_frame = matches!(marker, 0xC0..=0xCF) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if start_of_frame {
            let frame = jpeg.get(at + 4..at + 2 + length)?;
            let height = u16::from_be_bytes([*frame.get(1)?, *frame.get(2)?]) as u32;
            let width = u16::from_be_bytes([*frame.get(3)?, *frame.get(4)?]) as u32;
            return Some((width, height, *frame.get(5)?));
        }
        at += 2 + length;
    }
    None
}

/// Where a `w` × `h` pixel photo sits, centred in `photo_box`, in mm.
fn placed(photo_box: &Rect, w: u32, h: u32, ppi: u32) -> Rect {
    let (pw, ph) = (
        w as f64 / ppi as f64 * MM_PER_INCH,
        h as f64 / ppi as f64 * MM_PER_INCH,
    );
    // An image never larger than its box (a wrongly sized cell is shrunk).
    let scale = (photo_box.w / pw).min(photo_box.h / ph).min(1.0);
    let (pw, ph) = (pw * scale, ph * scale);
    Rect {
        x: photo_box.x + (photo_box.w - pw) / 2.0,
        y: photo_box.y + (photo_box.h - ph) / 2.0,
        w: pw,
        h: ph,
    }
}

/// A line of text on a page: where, how big and how aligned.
struct TextLine {
    text: String,
    /// x in mm: the left end, the centre or the right end (`align`).
    x: f64,
    baseline: f64,
    pt: f64,
    align: Align,
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Centre,
    Right,
}

impl TextLine {
    /// The left end of the line in mm, measured with `face`.
    fn left(&self, face: &Typeface) -> f64 {
        let width = face.width(&self.text, self.pt as f32) as f64 / PT_PER_MM;
        match self.align {
            Align::Left => self.x,
            Align::Centre => self.x - width / 2.0,
            Align::Right => self.x - width,
        }
    }
}

/// The text lines of page `page` (0-based) of `pages`.
fn page_lines(
    sheet: &ProofSheet,
    layout: &Layout,
    page: usize,
    captions: &[String],
    values: &PageValues,
    face: &Typeface,
) -> Vec<TextLine> {
    let [_, right, _, left] = sheet.margins_mm;
    let width = layout.page_width_mm - left - right;
    let text_pt = sheet.text_pt;
    let fit =
        |text: &str, pt: f64, max_mm: f64| face.fit(text, pt as f32, (max_mm * PT_PER_MM) as f32);
    let mut lines = Vec::new();
    let pages = layout.pages.len();
    if let (Some(baseline), Some(header)) = (layout.header_baseline, sheet.header.as_deref()) {
        lines.push(TextLine {
            text: fit(&page_text(header, values, page + 1, pages), text_pt, width),
            x: left,
            baseline,
            pt: text_pt,
            align: Align::Left,
        });
    }
    if let Some(baseline) = layout.footer_baseline {
        let number = sheet
            .page_numbers
            .then(|| format!("{} / {}", page + 1, pages));
        let number_mm = number
            .as_deref()
            .map(|n| face.width(n, text_pt as f32) as f64 / PT_PER_MM + 4.0)
            .unwrap_or(0.0);
        if let Some(footer) = sheet.footer.as_deref().filter(|f| !f.trim().is_empty()) {
            lines.push(TextLine {
                text: fit(
                    &page_text(footer, values, page + 1, pages),
                    text_pt,
                    width - number_mm,
                ),
                x: left,
                baseline,
                pt: text_pt,
                align: Align::Left,
            });
        }
        if let Some(number) = number {
            lines.push(TextLine {
                text: number,
                x: layout.page_width_mm - right,
                baseline,
                pt: text_pt,
                align: Align::Right,
            });
        }
    }
    for cell in &layout.pages[page].cells {
        if let (Some(baseline), Some(caption)) = (cell.caption_baseline, captions.get(cell.photo)) {
            lines.push(TextLine {
                text: fit(caption, sheet.caption_pt, cell.cell.w),
                x: cell.cell.x + cell.cell.w / 2.0,
                baseline,
                pt: sheet.caption_pt,
                align: Align::Centre,
            });
        }
    }
    lines.retain(|line| !line.text.trim().is_empty());
    lines
}

/// Length of a crop mark (mm) and its stroke width (pt).
const MARK_LENGTH_MM: f64 = 3.0;
const MARK_WIDTH_PT: f64 = 0.25;

/// The crop marks of page `page` as line segments `[x0, y0, x1, y1]` in
/// millimetres (origin top left): at each corner of each cell one horizontal
/// and one vertical tick pointing away from the cell. They are at most half a
/// gap long, so the ticks of neighbouring cells never meet or reach a
/// neighbour's photo; without room for a visible tick (under 0.5 mm) there
/// are none. Empty when the sheet asks for none.
pub fn crop_marks(sheet: &ProofSheet, layout: &Layout, page: usize) -> Vec<[f64; 4]> {
    let length = MARK_LENGTH_MM.min(sheet.gap_mm / 2.0);
    if !sheet.crop_marks || length < 0.5 {
        return Vec::new();
    }
    let mut marks = Vec::new();
    for cell in &layout.pages[page].cells {
        let c = cell.cell;
        for (x, dx) in [(c.x, -length), (c.x + c.w, length)] {
            for (y, dy) in [(c.y, -length), (c.y + c.h, length)] {
                marks.push([x, y, x + dx, y]);
                marks.push([x, y, x, y + dy]);
            }
        }
    }
    marks
}

/// The finished sheet: the PDF and/or one JPEG per page, as asked.
pub struct Sheet {
    pub pdf: Option<Vec<u8>>,
    pub jpegs: Vec<Vec<u8>>,
}

/// Lays out and writes the sheet. `cells[i]` is photo `i` rendered by the
/// export (none: it failed and its cell stays empty); `captions[i]` its
/// caption. `generated`: some photo has AI-generated areas, so the files
/// carry the IPTC Digital Source Type marking.
pub fn write_sheet(
    sheet: &ProofSheet,
    cells: &[Option<CellImage>],
    captions: &[String],
    values: &PageValues,
    generated: bool,
) -> Result<Sheet, String> {
    let layout = sheet.layout(cells.len())?;
    let texts = captions
        .iter()
        .map(String::as_str)
        .chain(sheet.header.as_deref())
        .chain(sheet.footer.as_deref())
        .chain([values.job.as_str(), values.date.as_str(), "…/0123456789"]);
    let face = Typeface::for_text(sheet.font_family.as_deref(), texts)
        .ok_or("No installed font can be used for the proof sheet's text.")?;
    let infos: Vec<Option<(u32, u32, u8)>> = cells
        .iter()
        .map(|cell| cell.as_ref().and_then(|c| jpeg_info(&c.jpeg)))
        .collect();
    let pdf = sheet.pdf.then(|| {
        write_pdf(
            sheet, &layout, cells, &infos, captions, values, &face, generated,
        )
    });
    let jpegs = if sheet.jpeg {
        (0..layout.pages.len())
            .map(|page| {
                render_page(
                    sheet, &layout, page, cells, captions, values, &face, generated,
                )
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    Ok(Sheet { pdf, jpegs })
}

fn num(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".into()
    } else {
        text.into()
    }
}

/// The Windows-1252 code of `c` (the invisible text layer's encoding).
fn win_ansi(c: char) -> Option<u8> {
    let code = c as u32;
    if (0x20..=0x7E).contains(&code) || (0xA0..=0xFF).contains(&code) {
        return Some(code as u8);
    }
    const HIGH: [(char, u8); 27] = [
        ('€', 0x80),
        ('‚', 0x82),
        ('ƒ', 0x83),
        ('„', 0x84),
        ('…', 0x85),
        ('†', 0x86),
        ('‡', 0x87),
        ('ˆ', 0x88),
        ('‰', 0x89),
        ('Š', 0x8A),
        ('‹', 0x8B),
        ('Œ', 0x8C),
        ('Ž', 0x8E),
        ('‘', 0x91),
        ('’', 0x92),
        ('“', 0x93),
        ('”', 0x94),
        ('•', 0x95),
        ('–', 0x96),
        ('—', 0x97),
        ('˜', 0x98),
        ('™', 0x99),
        ('š', 0x9A),
        ('›', 0x9B),
        ('œ', 0x9C),
        ('ž', 0x9E),
        ('Ÿ', 0x9F),
    ];
    HIGH.iter().find(|(h, _)| *h == c).map(|(_, b)| *b)
}

fn char_of_win_ansi(code: u8) -> Option<char> {
    // Every character win_ansi maps lies below U+2200.
    (32u32..0x2200)
        .filter_map(char::from_u32)
        .find(|c| win_ansi(*c) == Some(code))
}

fn pdf_string(bytes: &[u8]) -> String {
    let mut out = String::from("(");
    for &b in bytes {
        match b {
            b'(' | b')' | b'\\' => {
                out.push('\\');
                out.push(b as char);
            }
            0x20..=0x7E => out.push(b as char),
            _ => {
                let _ = write!(out, "\\{b:03o}");
            }
        }
    }
    out.push(')');
    out
}

/// A text string of the document information (UTF-16BE with BOM).
fn pdf_text(text: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in text.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
    out
}

fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// A PDF being written: numbered objects, then the cross-reference table.
struct PdfWriter {
    out: Vec<u8>,
    offsets: Vec<usize>,
}

impl PdfWriter {
    fn new() -> Self {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        out.reserve(1 << 20);
        Self {
            out,
            offsets: vec![0],
        }
    }
    /// Reserves the next object number.
    fn reserve(&mut self) -> usize {
        self.offsets.push(0);
        self.offsets.len() - 1
    }
    fn object(&mut self, id: usize, body: &str) {
        self.offsets[id] = self.out.len();
        self.out
            .extend_from_slice(format!("{id} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    fn stream(&mut self, id: usize, dict: &str, data: &[u8]) {
        self.offsets[id] = self.out.len();
        self.out.extend_from_slice(
            format!("{id} 0 obj\n<<{dict} /Length {}>>\nstream\n", data.len()).as_bytes(),
        );
        self.out.extend_from_slice(data);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
    }
    fn finish(mut self, root: usize, info: usize) -> Vec<u8> {
        let xref = self.out.len();
        let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len());
        for offset in &self.offsets[1..] {
            let _ = writeln!(table, "{offset:010} 00000 n ");
        }
        let id = blake3::hash(&self.out).to_hex()[..32].to_string();
        let _ = write!(
            table,
            "trailer\n<< /Size {} /Root {root} 0 R /Info {info} 0 R /ID [<{id}> <{id}>] >>\nstartxref\n{xref}\n%%EOF\n",
            self.offsets.len()
        );
        self.out.extend_from_slice(table.as_bytes());
        self.out
    }
}

const XMP_GENERATED: &str = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:Iptc4xmpExt="http://iptc.org/std/Iptc4xmpExt/2008-02-29/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" Iptc4xmpExt:DigitalSourceType="SOURCE" xmp:CreatorTool="Rawtonic"/></rdf:RDF></x:xmpmeta>
<?xpacket end="w"?>"#;

#[allow(clippy::too_many_arguments)]
fn write_pdf(
    sheet: &ProofSheet,
    layout: &Layout,
    cells: &[Option<CellImage>],
    infos: &[Option<(u32, u32, u8)>],
    captions: &[String],
    values: &PageValues,
    face: &Typeface,
    generated: bool,
) -> Vec<u8> {
    let mut pdf = PdfWriter::new();
    let catalog = pdf.reserve();
    let pages_id = pdf.reserve();
    let font = pdf.reserve();
    let icc = pdf.reserve();
    let info = pdf.reserve();
    let page_h = layout.page_height_mm * PT_PER_MM;

    // The invisible text layer: Helvetica in WinAnsi, its widths those of
    // the face the visible outlines are drawn with, so selections fit them.
    let widths: Vec<String> = (32u8..=255)
        .map(|code| {
            char_of_win_ansi(code)
                .map(|c| num(face.advance(c, 1000.0) as f64))
                .unwrap_or_else(|| "0".into())
        })
        .collect();
    pdf.object(
        font,
        &format!(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 /Widths [{}] >>",
            widths.join(" ")
        ),
    );
    pdf.stream(
        icc,
        " /N 3 /Alternate /DeviceRGB /Filter /FlateDecode",
        &deflate(rawtonic_engine::icc_profile::srgb()),
    );

    let mut page_ids = Vec::new();
    for page in 0..layout.pages.len() {
        let page_id = pdf.reserve();
        let content_id = pdf.reserve();
        page_ids.push(page_id);
        let mut content = String::new();
        let mut images = Vec::new();
        for cell in &layout.pages[page].cells {
            let (Some(image), Some((w, h, components))) = (&cells[cell.photo], infos[cell.photo])
            else {
                continue;
            };
            let at = placed(&cell.photo_box, w, h, sheet.ppi);
            let name = format!("Im{}", cell.photo);
            let id = pdf.reserve();
            let space = if components == 1 {
                "/DeviceGray".to_string()
            } else {
                format!("[/ICCBased {icc} 0 R]")
            };
            pdf.stream(
                id,
                &format!(
                    " /Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace {space} /BitsPerComponent 8 /Filter /DCTDecode"
                ),
                &image.jpeg,
            );
            images.push(format!("/{name} {id} 0 R"));
            let _ = writeln!(
                content,
                "q {} 0 0 {} {} {} cm /{name} Do Q",
                num(at.w * PT_PER_MM),
                num(at.h * PT_PER_MM),
                num(at.x * PT_PER_MM),
                num(page_h - (at.y + at.h) * PT_PER_MM)
            );
        }
        let marks = crop_marks(sheet, layout, page);
        if !marks.is_empty() {
            let _ = writeln!(content, "{} w 0 G", num(MARK_WIDTH_PT));
            for [x0, y0, x1, y1] in marks {
                let _ = writeln!(
                    content,
                    "{} {} m {} {} l S",
                    num(x0 * PT_PER_MM),
                    num(page_h - y0 * PT_PER_MM),
                    num(x1 * PT_PER_MM),
                    num(page_h - y1 * PT_PER_MM)
                );
            }
        }
        let lines = page_lines(sheet, layout, page, captions, values, face);
        content.push_str("0 g\n");
        for line in &lines {
            let x = (line.left(face) * PT_PER_MM) as f32;
            let y = (page_h - line.baseline * PT_PER_MM) as f32;
            for op in face.outline(&line.text, line.pt as f32, x, y) {
                let _ = match op {
                    PathOp::Move(x, y) => {
                        writeln!(content, "{} {} m", num(x as f64), num(y as f64))
                    }
                    PathOp::Line(x, y) => {
                        writeln!(content, "{} {} l", num(x as f64), num(y as f64))
                    }
                    PathOp::Cubic(a, b, c, d, e, f) => writeln!(
                        content,
                        "{} {} {} {} {} {} c",
                        num(a as f64),
                        num(b as f64),
                        num(c as f64),
                        num(d as f64),
                        num(e as f64),
                        num(f as f64)
                    ),
                    PathOp::Close => writeln!(content, "h"),
                };
            }
            content.push_str("f\n");
        }
        // Selectable, searchable copy of the text (render mode 3: invisible),
        // stretched to the drawn line. Characters outside WinAnsi stand in as
        // `?`, and the line's real text goes with it as ActualText, so copying
        // and searching give the names in any script.
        for line in &lines {
            let bytes: Vec<u8> = line
                .text
                .chars()
                .map(|c| win_ansi(c).unwrap_or(b'?'))
                .collect();
            if bytes.is_empty() {
                continue;
            }
            let layer_width: f32 = bytes
                .iter()
                .filter_map(|b| char_of_win_ansi(*b))
                .map(|c| face.advance(c, line.pt as f32))
                .sum();
            let drawn = face.width(&line.text, line.pt as f32);
            let stretch = if layer_width > 0.0 && drawn > 0.0 {
                (drawn / layer_width * 100.0) as f64
            } else {
                100.0
            };
            let exact = line.text.chars().all(|c| win_ansi(c).is_some());
            if !exact {
                let _ = write!(
                    content,
                    "/Span << /ActualText {} >> BDC ",
                    pdf_text(&line.text)
                );
            }
            let _ = write!(
                content,
                "BT /F1 {} Tf 3 Tr {} Tz {} {} Td {} Tj ET",
                num(line.pt),
                num(stretch),
                num(line.left(face) * PT_PER_MM),
                num(page_h - line.baseline * PT_PER_MM),
                pdf_string(&bytes)
            );
            content.push_str(if exact { "\n" } else { " EMC\n" });
        }
        pdf.stream(
            content_id,
            " /Filter /FlateDecode",
            &deflate(content.as_bytes()),
        );
        pdf.object(
            page_id,
            &format!(
                "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {} {}] /Resources << /Font << /F1 {font} 0 R >> /XObject << {} >> >> /Contents {content_id} 0 R >>",
                num(layout.page_width_mm * PT_PER_MM),
                num(page_h),
                images.join(" ")
            ),
        );
    }
    pdf.object(
        pages_id,
        &format!(
            "<< /Type /Pages /Kids [{}] /Count {} >>",
            page_ids
                .iter()
                .map(|id| format!("{id} 0 R"))
                .collect::<Vec<_>>()
                .join(" "),
            page_ids.len()
        ),
    );
    let metadata = generated.then(|| {
        let id = pdf.reserve();
        let xmp = XMP_GENERATED.replace("SOURCE", rawtonic_metadata::DIGITAL_SOURCE_COMPOSITE_AI);
        pdf.stream(id, " /Type /Metadata /Subtype /XML", xmp.as_bytes());
        id
    });
    pdf.object(
        catalog,
        &format!(
            "<< /Type /Catalog /Pages {pages_id} 0 R{} >>",
            metadata
                .map(|id| format!(" /Metadata {id} 0 R"))
                .unwrap_or_default()
        ),
    );
    let title = if values.job.trim().is_empty() {
        "Proof sheet".to_string()
    } else {
        values.job.clone()
    };
    pdf.object(
        info,
        &format!(
            "<< /Title {} /Creator (Rawtonic) /Producer (Rawtonic) >>",
            pdf_text(&title)
        ),
    );
    pdf.finish(catalog, info)
}

/// Page `page` as an sRGB JPEG at the sheet's ppi.
#[allow(clippy::too_many_arguments)]
fn render_page(
    sheet: &ProofSheet,
    layout: &Layout,
    page: usize,
    cells: &[Option<CellImage>],
    captions: &[String],
    values: &PageValues,
    face: &Typeface,
    generated: bool,
) -> Result<Vec<u8>, String> {
    let px_per_mm = sheet.ppi as f64 / MM_PER_INCH;
    let (w, h) = (
        (layout.page_width_mm * px_per_mm).round() as u32,
        (layout.page_height_mm * px_per_mm).round() as u32,
    );
    let mut canvas = image::RgbImage::from_pixel(w, h, image::Rgb([255, 255, 255]));
    for cell in &layout.pages[page].cells {
        let Some(image) = &cells[cell.photo] else {
            continue;
        };
        let decoded = image::load_from_memory_with_format(&image.jpeg, image::ImageFormat::Jpeg)
            .map_err(|e| format!("A rendered photo could not be read: {e}"))?
            .to_rgb8();
        let at = placed(
            &cell.photo_box,
            decoded.width(),
            decoded.height(),
            sheet.ppi,
        );
        let (tw, th) = (
            ((at.w * px_per_mm).round() as u32).max(1),
            ((at.h * px_per_mm).round() as u32).max(1),
        );
        let decoded = if (tw, th) == decoded.dimensions() {
            decoded
        } else {
            image::imageops::resize(&decoded, tw, th, image::imageops::FilterType::Lanczos3)
        };
        image::imageops::replace(
            &mut canvas,
            &decoded,
            (at.x * px_per_mm).round() as i64,
            (at.y * px_per_mm).round() as i64,
        );
    }
    let stroke = ((MARK_WIDTH_PT / 72.0 * sheet.ppi as f64).round() as i64).max(1);
    for [x0, y0, x1, y1] in crop_marks(sheet, layout, page) {
        // Axis-parallel ticks as filled rectangles `stroke` pixels wide.
        let (ax, bx) = (x0.min(x1) * px_per_mm, x0.max(x1) * px_per_mm);
        let (ay, by) = (y0.min(y1) * px_per_mm, y0.max(y1) * px_per_mm);
        let left = ax.round() as i64 - if ax == bx { stroke / 2 } else { 0 };
        let top = ay.round() as i64 - if ay == by { stroke / 2 } else { 0 };
        let right = if ax == bx {
            left + stroke
        } else {
            bx.round() as i64
        };
        let bottom = if ay == by {
            top + stroke
        } else {
            by.round() as i64
        };
        for py in top.max(0)..bottom.min(h as i64) {
            for px in left.max(0)..right.min(w as i64) {
                canvas.put_pixel(px as u32, py as u32, image::Rgb([0, 0, 0]));
            }
        }
    }
    for line in page_lines(sheet, layout, page, captions, values, face) {
        let size = (line.pt / 72.0 * sheet.ppi as f64) as f32;
        let x = (line.left(face) * px_per_mm) as f32;
        let y = (line.baseline * px_per_mm) as f32;
        face.rasterize(&line.text, size, x, y, |px, py, coverage| {
            if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                let pixel = canvas.get_pixel_mut(px as u32, py as u32);
                let keep = 1.0 - coverage.clamp(0.0, 1.0);
                for c in pixel.0.iter_mut() {
                    *c = (*c as f32 * keep).round() as u8;
                }
            }
        });
    }
    let settings: crate::export_processing::ExportSettings =
        serde_json::from_value(serde_json::json!({
            "jpegQuality": sheet.jpeg_quality.clamp(1, 100),
            "keepMetadata": false,
            "stripGps": true,
        }))
        .map_err(|e| e.to_string())?;
    let metadata = generated
        .then(crate::exif_processing::ExportMetadata::marking_only)
        .transpose()?;
    crate::export_processing::encode_with_metadata(
        image::DynamicImage::ImageRgb8(canvas),
        "jpg",
        &settings,
        metadata.as_ref(),
    )
}

/// The quality the photos are rendered at for the sheet (placed in the PDF
/// as they are, so they are compressed only once).
pub const CELL_QUALITY: u8 = 95;

/// What each photo of a sheet with `layout` is exported with: an sRGB JPEG
/// fitted to the photo box at the sheet's ppi, sharpened for that size,
/// with the export's watermark, without metadata (the AI marking stays).
pub fn cell_settings(
    layout: &Layout,
    watermark: Option<crate::export_processing::WatermarkSettings>,
    output_sharpening: Option<crate::output_sharpening::OutputSharpening>,
) -> crate::export_processing::ExportSettings {
    use crate::export_processing::{ExportSettings, ResizeMode, ResizeOptions};
    ExportSettings {
        jpeg_quality: CELL_QUALITY,
        tiff_bit_depth: Default::default(),
        resize: Some(ResizeOptions {
            mode: ResizeMode::FitBox,
            value: layout.box_px.0 as f64,
            second_value: layout.box_px.1 as f64,
            dont_enlarge: false,
        }),
        border: None,
        pad: None,
        keep_metadata: false,
        color_space: Default::default(),
        preserve_timestamps: false,
        strip_gps: true,
        keep_creator_contact: false,
        filename_template: None,
        // One file per photo, numbered in the sheet's order.
        name_template: Some("cell-{seq:auto}".into()),
        job_name: None,
        recipe_name: None,
        sequence_start: Some(1),
        watermark,
        export_masks: false,
        preserve_folders: false,
        destination_type: None,
        subfolder: None,
        output_sharpening,
        super_resolution: false,
    }
}

/// A photo's caption: `template` in the naming tokens filled for it (as an
/// export name is, without the file name checks); a template that does not
/// parse gives the original's name.
pub fn caption(template: &str, naming: &rawtonic_naming::Context) -> String {
    rawtonic_naming::render_text(template, naming)
        .unwrap_or_else(|_| naming.original.clone())
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// `name` made a safe file name stem ("Proof sheet" when nothing is left).
pub fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim();
    let cleaned: String = cleaned.chars().take(120).collect();
    if cleaned.is_empty() {
        "Proof sheet".into()
    } else {
        cleaned
    }
}

/// Writes the sheet into `folder` as `stem.pdf` and `stem-01.jpg`, … (one
/// per page), never over a file: when any of the names is taken, all of
/// them get the next free number (`stem-2.pdf`, `stem-2-01.jpg`).
pub fn write_files(
    folder: &std::path::Path,
    stem: &str,
    sheet: &Sheet,
) -> Result<Vec<std::path::PathBuf>, String> {
    let width = sheet.jpegs.len().to_string().len().max(2);
    let names = |attempt: usize| -> Vec<(std::path::PathBuf, &[u8])> {
        let base = if attempt == 1 {
            stem.to_string()
        } else {
            format!("{stem}-{attempt}")
        };
        sheet
            .pdf
            .iter()
            .map(|pdf| (folder.join(format!("{base}.pdf")), pdf.as_slice()))
            .chain(sheet.jpegs.iter().enumerate().map(|(i, jpeg)| {
                (
                    folder.join(format!("{base}-{:0width$}.jpg", i + 1)),
                    jpeg.as_slice(),
                )
            }))
            .collect()
    };
    for attempt in 1..1000 {
        let files = names(attempt);
        if files.iter().any(|(path, _)| path.exists()) {
            continue;
        }
        let mut written = Vec::new();
        let mut taken = false;
        for (path, bytes) in &files {
            match crate::output::write_new_output(path, bytes) {
                Ok(()) => written.push(path.clone()),
                // Another program took the name meanwhile: what this
                // attempt wrote stays, the rest goes under the next number.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    taken = true;
                    break;
                }
                Err(e) => return Err(format!("{}: {e}", path.display())),
            }
        }
        if !taken {
            return Ok(written);
        }
    }
    Err("Too many proof sheets with this name in the folder.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4() -> ProofSheet {
        ProofSheet {
            page_width_mm: 210.0,
            page_height_mm: 297.0,
            margins_mm: [12.0, 12.0, 12.0, 12.0],
            columns: 4,
            rows: 5,
            gap_mm: 4.0,
            caption: Some("{seq} {orig}".into()),
            caption_pt: 8.0,
            header: Some("{job} · {date}".into()),
            footer: None,
            page_numbers: true,
            text_pt: 10.0,
            font_family: None,
            crop_marks: false,
            ppi: 300,
            pdf: true,
            jpeg: false,
            jpeg_quality: 92,
        }
    }

    #[test]
    fn photos_fill_pages_row_by_row() {
        let sheet = a4();
        let layout = sheet.layout(45).unwrap();
        assert_eq!(layout.pages.len(), 3);
        assert_eq!(
            layout
                .pages
                .iter()
                .map(|p| p.cells.len())
                .collect::<Vec<_>>(),
            [20, 20, 5]
        );
        let first = &layout.pages[0].cells;
        assert_eq!(first[0].photo, 0);
        assert_eq!(layout.pages[2].cells[4].photo, 44);
        // Row by row: photo 1 is right of photo 0, photo 4 below it.
        assert!(first[1].cell.x > first[0].cell.x && first[1].cell.y == first[0].cell.y);
        assert!(first[4].cell.y > first[0].cell.y && first[4].cell.x == first[0].cell.x);
        // Everything within the margins, cells not overlapping.
        let last = &first[19].cell;
        assert!((last.x + last.w - (210.0 - 12.0)).abs() < 1e-9);
        assert!(last.y + last.h <= 297.0 - 12.0);
        assert!(first[0].cell.y >= 12.0);
        assert!((first[1].cell.x - (first[0].cell.x + first[0].cell.w) - 4.0).abs() < 1e-9);
        // Caption below the photo, inside the cell.
        let cell = &first[0];
        let baseline = cell.caption_baseline.unwrap();
        assert!(baseline > cell.photo_box.y + cell.photo_box.h);
        assert!(baseline < cell.cell.y + cell.cell.h);
        // The pixel box is the photo box at 300 ppi.
        let expected = (cell.photo_box.w / 25.4 * 300.0).floor() as u32;
        assert_eq!(layout.box_px.0, expected);
    }

    #[test]
    fn an_empty_or_exact_selection_has_whole_pages() {
        let sheet = a4();
        assert_eq!(sheet.layout(20).unwrap().pages.len(), 1);
        assert_eq!(sheet.layout(21).unwrap().pages.len(), 2);
        let none = sheet.layout(0).unwrap();
        assert_eq!(none.pages.len(), 1);
        assert!(none.pages[0].cells.is_empty());
    }

    #[test]
    fn impossible_layouts_are_refused() {
        let mut sheet = a4();
        sheet.columns = 0;
        assert!(sheet.layout(1).is_err());
        let mut sheet = a4();
        sheet.margins_mm = [140.0, 12.0, 140.0, 12.0];
        assert!(sheet.layout(1).is_err());
        let mut sheet = a4();
        sheet.page_width_mm = f64::NAN;
        assert!(sheet.layout(1).is_err());
        let mut sheet = a4();
        sheet.pdf = false;
        assert!(sheet.layout(1).is_err());
    }

    #[test]
    fn no_caption_gives_the_photo_the_whole_cell() {
        let mut sheet = a4();
        sheet.caption = None;
        let layout = sheet.layout(1).unwrap();
        let cell = &layout.pages[0].cells[0];
        assert_eq!(cell.photo_box, cell.cell);
        assert_eq!(cell.caption_baseline, None);
    }

    #[test]
    fn page_text_fills_its_tokens() {
        let values = PageValues {
            job: "Smith wedding".into(),
            date: "2026-11-02".into(),
            count: 20,
        };
        assert_eq!(
            page_text(
                "{job} – {date} – {count} photos – {page}/{pages} {other}",
                &values,
                2,
                3
            ),
            "Smith wedding – 2026-11-02 – 20 photos – 2/3 {other}"
        );
    }

    #[test]
    fn jpeg_size_is_read_from_its_frame() {
        let image = image::DynamicImage::new_rgb8(37, 21);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        assert_eq!(jpeg_info(&bytes), Some((37, 21, 3)));
        assert_eq!(jpeg_info(b"not a jpeg"), None);
    }

    #[test]
    fn win_ansi_covers_latin_text_only() {
        assert_eq!(win_ansi('A'), Some(b'A'));
        assert_eq!(win_ansi('é'), Some(0xE9));
        assert_eq!(win_ansi('…'), Some(0x85));
        assert_eq!(win_ansi('漢'), None);
        assert_eq!(char_of_win_ansi(0x85), Some('…'));
        assert_eq!(char_of_win_ansi(0x81), None);
        assert_eq!(pdf_string(b"a(b)\\\x85"), "(a\\(b\\)\\\\\\205)");
    }

    #[test]
    fn sheet_files_never_replace_existing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let sheet = Sheet {
            pdf: Some(b"%PDF one".to_vec()),
            jpegs: vec![b"a".to_vec(), b"b".to_vec()],
        };
        let first = write_files(dir.path(), "Smith", &sheet).unwrap();
        let names = |paths: &[std::path::PathBuf]| {
            paths
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&first), ["Smith.pdf", "Smith-01.jpg", "Smith-02.jpg"]);
        // Only one of the names taken: all of them move on together.
        std::fs::remove_file(dir.path().join("Smith.pdf")).unwrap();
        let second = write_files(dir.path(), "Smith", &sheet).unwrap();
        assert_eq!(
            names(&second),
            ["Smith-2.pdf", "Smith-2-01.jpg", "Smith-2-02.jpg"]
        );
        assert_eq!(
            std::fs::read(dir.path().join("Smith-01.jpg")).unwrap(),
            b"a"
        );
        assert_eq!(file_stem(" a/b:c. "), "a-b-c");
        assert_eq!(file_stem("///"), "---");
        assert_eq!(file_stem(" . "), "Proof sheet");
    }

    #[test]
    fn captions_fill_the_naming_tokens_as_free_text() {
        let mut naming = rawtonic_naming::Context::new("DSC_0042.NEF", 7);
        naming.values.insert("rating".into(), "4".into());
        assert_eq!(
            caption("{seq} · {orig} · {rating}/5", &naming),
            "7 · DSC_0042.NEF · 4/5"
        );
        assert_eq!(caption("{nonsense}", &naming), "DSC_0042.NEF");
        let layout = a4().layout(3).unwrap();
        let settings = cell_settings(&layout, None, None);
        let resize = settings.resize.unwrap();
        assert_eq!(
            (resize.value as u32, resize.second_value as u32),
            layout.box_px
        );
        assert_eq!(
            crate::export_processing::calculate_resize_target(6000, 4000, &resize).0,
            layout.box_px.0
        );
    }

    #[test]
    fn photos_are_centred_and_never_larger_than_their_box() {
        let photo_box = Rect {
            x: 10.0,
            y: 20.0,
            w: 40.0,
            h: 30.0,
        };
        // 300 ppi: 236 px = 19.98 mm.
        let at = placed(&photo_box, 236, 354, 300);
        assert!((at.x + at.w / 2.0 - 30.0).abs() < 1e-9);
        assert!((at.y + at.h / 2.0 - 35.0).abs() < 1e-9);
        assert!(at.h <= 30.0 + 1e-9);
        let big = placed(&photo_box, 4000, 3000, 300);
        assert!(big.w <= 40.0 + 1e-9 && big.h <= 30.0 + 1e-9);
    }
}
