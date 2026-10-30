//! Atomic sidecar IO. Original image files are never passed to this API.
pub mod no_clobber;
pub mod provenance;
pub mod resources;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

pub const XMP: &str = "http://ns.adobe.com/xap/1.0/";
pub const DC: &str = "http://purl.org/dc/elements/1.1/";
pub const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const LR: &str = "http://ns.adobe.com/lightroom/1.0/";
/// Rawtonic's own properties (flag, recipe), written with the `rawtonic:` prefix.
pub const RT: &str = "https://rawtonic.cc/ns/1.0/";
/// The namespace written before the rename to Rawtonic (as Re-touch). Still
/// read, and replaced by `RT` whenever a sidecar is rewritten.
pub const FORMER_RT: &str = "https://retouch.photo/ns/1.0/";
pub const PHOTOSHOP: &str = "http://ns.adobe.com/photoshop/1.0/";
pub const IPTC_CORE: &str = "http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/";
pub const XMP_RIGHTS: &str = "http://ns.adobe.com/xap/1.0/rights/";
pub const IPTC_EXT: &str = "http://iptc.org/std/Iptc4xmpExt/2008-02-29/";
/// IPTC Digital Source Type of a photo with areas made by a generative model
/// (object removal, generative fill): the machine-readable marking the EU AI
/// Act (Art. 50(2)) asks of such output.
pub const DIGITAL_SOURCE_COMPOSITE_AI: &str =
    "http://cv.iptc.org/newscodes/digitalsourcetype/compositeWithTrainedAlgorithmicMedia";

/// How an IPTC field is stored in XMP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IptcKind {
    /// A simple property (attribute or element text).
    Simple,
    /// A language alternative (`rdf:Alt`), written as `x-default`.
    Alt,
    /// A field of `Iptc4xmpCore:CreatorContactInfo`.
    Contact,
}

/// The IPTC Core fields Rawtonic keeps (Capture One's and Lightroom's IPTC
/// panels): key in [`Metadata::iptc`], namespace, XMP name, kind. Title,
/// description, creator and copyright are Dublin Core fields of `Metadata`.
pub const IPTC_FIELDS: &[(&str, &str, &str, IptcKind)] = &[
    ("headline", PHOTOSHOP, "Headline", IptcKind::Simple),
    ("location", IPTC_CORE, "Location", IptcKind::Simple),
    ("city", PHOTOSHOP, "City", IptcKind::Simple),
    ("state", PHOTOSHOP, "State", IptcKind::Simple),
    ("country", PHOTOSHOP, "Country", IptcKind::Simple),
    ("countryCode", IPTC_CORE, "CountryCode", IptcKind::Simple),
    (
        "jobId",
        PHOTOSHOP,
        "TransmissionReference",
        IptcKind::Simple,
    ),
    ("instructions", PHOTOSHOP, "Instructions", IptcKind::Simple),
    ("credit", PHOTOSHOP, "Credit", IptcKind::Simple),
    ("source", PHOTOSHOP, "Source", IptcKind::Simple),
    ("usageTerms", XMP_RIGHTS, "UsageTerms", IptcKind::Alt),
    (
        "creatorJobTitle",
        PHOTOSHOP,
        "AuthorsPosition",
        IptcKind::Simple,
    ),
    ("creatorEmail", IPTC_CORE, "CiEmailWork", IptcKind::Contact),
    ("creatorUrl", IPTC_CORE, "CiUrlWork", IptcKind::Contact),
    ("creatorPhone", IPTC_CORE, "CiTelWork", IptcKind::Contact),
    (
        "creatorAddress",
        IPTC_CORE,
        "CiAdrExtadr",
        IptcKind::Contact,
    ),
    ("creatorCity", IPTC_CORE, "CiAdrCity", IptcKind::Contact),
    ("creatorRegion", IPTC_CORE, "CiAdrRegion", IptcKind::Contact),
    (
        "creatorPostalCode",
        IPTC_CORE,
        "CiAdrPcode",
        IptcKind::Contact,
    ),
    ("creatorCountry", IPTC_CORE, "CiAdrCtry", IptcKind::Contact),
];

/// The XMP field of an IPTC key.
pub fn iptc_field(key: &str) -> Option<(&'static str, &'static str, IptcKind)> {
    IPTC_FIELDS
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|(_, ns, name, kind)| (*ns, *name, *kind))
}

/// The IPTC key of a top-level XMP property (not contact fields).
fn iptc_key(namespace: Option<&str>, name: &str) -> Option<&'static str> {
    IPTC_FIELDS
        .iter()
        .find(|(_, ns, n, kind)| Some(*ns) == namespace && *n == name && *kind != IptcKind::Contact)
        .map(|(k, ..)| *k)
}

fn contact_key(namespace: Option<&str>, name: &str) -> Option<&'static str> {
    IPTC_FIELDS
        .iter()
        .find(|(_, ns, n, kind)| Some(*ns) == namespace && *n == name && *kind == IptcKind::Contact)
        .map(|(k, ..)| *k)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Metadata {
    pub rating: u8,
    pub label: String,
    pub flag: i8,
    pub keywords: Vec<String>,
    pub title: String,
    pub description: String,
    pub creator: String,
    pub copyright: String,
    /// IPTC Core fields by the keys of [`IPTC_FIELDS`]; empty ones are absent.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub iptc: BTreeMap<String, String>,
}

impl Metadata {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.rating <= 5, "Rating must be 0–5");
        ensure!((-1..=1).contains(&self.flag), "Flag must be -1, 0 or 1");
        ensure!(self.keywords.len() <= 10_000, "Too many keywords");
        for value in [
            &self.label,
            &self.title,
            &self.description,
            &self.creator,
            &self.copyright,
        ]
        .into_iter()
        .chain(self.keywords.iter())
        .chain(self.iptc.values())
        {
            ensure!(value.len() <= 1_000_000, "Metadata field too large");
            ensure!(
                !value
                    .chars()
                    .any(|c| c < '\u{20}' && !['\n', '\t', '\r'].contains(&c)),
                "Metadata contains invalid XML controls"
            );
        }
        for key in self.iptc.keys() {
            ensure!(iptc_field(key).is_some(), "Unknown IPTC field {key}");
        }
        Ok(())
    }

    /// Sets an IPTC field; an empty value removes it.
    pub fn set_iptc(&mut self, key: &str, value: &str) {
        let value = value.trim();
        if value.is_empty() {
            self.iptc.remove(key);
        } else {
            self.iptc.insert(key.to_string(), value.to_string());
        }
    }
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Same-directory temporary file, fsync, atomic replace. Reject symlink targets.
/// A replaced file keeps its permissions (the temporary file's are private).
pub fn atomic_write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    let parent = path.parent().context("File needs a parent directory")?;
    let replaced = fs::symlink_metadata(path).ok();
    if let Some(meta) = &replaced {
        ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "Refusing to replace a symlink or non-file"
        );
    }
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes.as_ref())?;
    file.as_file().sync_all()?;
    if let Some(meta) = &replaced {
        file.as_file().set_permissions(meta.permissions())?;
    }
    file.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn managed(namespace: Option<&str>, name: &str) -> bool {
    match namespace {
        Some(XMP) => ["Rating", "Label"].contains(&name),
        Some(DC) => ["subject", "title", "description", "creator", "rights"].contains(&name),
        Some(LR) => name == "hierarchicalSubject",
        Some(RT | FORMER_RT) => ["Flag", "Recipe"].contains(&name),
        _ => false,
    }
}

/// Whether a packet says anything about the culling flag: Rawtonic's own
/// `Flag` property, or a rejected rating (`xmp:Rating` -1). Other editors
/// rewriting a sidecar may leave the flag out; reading such a packet back
/// keeps the library's flag rather than taking the absent one as "unflagged".
pub fn carries_flag(text: &str) -> Result<bool> {
    ensure!(text.len() <= 16 * 1024 * 1024, "XMP exceeds 16 MiB limit");
    let doc = roxmltree::Document::parse(text)?;
    let flag = |namespace: Option<&str>, name: &str| {
        matches!(namespace, Some(RT | FORMER_RT)) && name == "Flag"
    };
    let rejected = |namespace: Option<&str>, name: &str, value: &str| {
        namespace == Some(XMP) && name == "Rating" && value.trim() == "-1"
    };
    Ok(doc.descendants().filter(|n| n.is_element()).any(|node| {
        let tag = node.tag_name();
        flag(tag.namespace(), tag.name())
            || rejected(tag.namespace(), tag.name(), node.text().unwrap_or_default())
            || node.attributes().any(|attr| {
                flag(attr.namespace(), attr.name())
                    || rejected(attr.namespace(), attr.name(), attr.value())
            })
    }))
}

pub fn read_xmp(text: &str) -> Result<(Metadata, Option<serde_json::Value>)> {
    ensure!(text.len() <= 16 * 1024 * 1024, "XMP exceeds 16 MiB limit");
    let doc = roxmltree::Document::parse(text)?;
    let mut result = Metadata::default();
    let mut recipe = None;
    // A rejected rating (another editor's reject) wins over a flag property
    // in any order: Rawtonic writes ratings 0–5 and its flag apart, so a -1
    // rating beside a pick is an outside change the flag did not follow.
    let mut rejected = false;
    for node in doc.descendants().filter(|n| n.is_element()) {
        let name = node.tag_name();
        for attr in node.attributes() {
            match (attr.namespace(), attr.name()) {
                (Some(XMP), "Rating") => {
                    let rating: i8 = attr.value().parse()?;
                    result.rating = rating.max(0) as u8;
                    rejected |= rating == -1;
                }
                (Some(XMP), "Label") => result.label = attr.value().into(),
                (Some(RT | FORMER_RT), "Flag") => result.flag = attr.value().parse()?,
                (namespace, name) => {
                    let key = iptc_key(namespace, name).or_else(|| {
                        node.ancestors()
                            .any(|a| a.has_tag_name((IPTC_CORE, "CreatorContactInfo")))
                            .then(|| contact_key(namespace, name))
                            .flatten()
                            .or_else(|| {
                                node.has_tag_name((IPTC_CORE, "CreatorContactInfo"))
                                    .then(|| contact_key(namespace, name))
                                    .flatten()
                            })
                    });
                    if let Some(key) = key {
                        result.set_iptc(key, attr.value());
                    }
                }
            }
        }
        let value = || {
            node.descendants()
                .find(|n| {
                    n.is_element()
                        && n.tag_name().namespace() == Some(RDF)
                        && n.tag_name().name() == "li"
                })
                .and_then(|n| n.text())
                .or_else(|| node.text())
                .unwrap_or("")
                .to_string()
        };
        match (name.namespace(), name.name()) {
            (Some(XMP), "Rating") => {
                let rating: i8 = value().parse()?;
                result.rating = rating.max(0) as u8;
                rejected |= rating == -1;
            }
            (Some(XMP), "Label") => result.label = value(),
            (Some(RT | FORMER_RT), "Flag") => result.flag = value().parse()?,
            (Some(RT | FORMER_RT), "Recipe") => recipe = Some(serde_json::from_str(&value())?),
            (Some(DC), "title") => result.title = value(),
            (Some(DC), "description") => result.description = value(),
            (Some(DC), "creator") => result.creator = value(),
            (Some(DC), "rights") => result.copyright = value(),
            (namespace, tag)
                if iptc_key(namespace, tag).is_some()
                    || (contact_key(namespace, tag).is_some()
                        && node
                            .ancestors()
                            .any(|a| a.has_tag_name((IPTC_CORE, "CreatorContactInfo")))) =>
            {
                let key = iptc_key(namespace, tag).or_else(|| contact_key(namespace, tag));
                if let Some(key) = key {
                    result.set_iptc(key, &value());
                }
            }
            (Some(DC), "subject") | (Some(LR), "hierarchicalSubject") => {
                for li in node.descendants().filter(|n| n.has_tag_name((RDF, "li"))) {
                    if let Some(s) = li.text()
                        && !result.keywords.iter().any(|k| k == s)
                    {
                        result.keywords.push(s.into());
                    }
                }
            }
            _ => {}
        }
    }
    if rejected {
        result.flag = -1;
    }
    result.validate()?;
    Ok((result, recipe))
}

/// Namespace declarations for [`iptc_elements`]; none without IPTC, so
/// sidecars and exports without IPTC stay as they were.
fn iptc_namespaces(metadata: &Metadata) -> String {
    if metadata.iptc.values().all(|v| v.trim().is_empty()) {
        return String::new();
    }
    format!(
        " xmlns:photoshop=\"{PHOTOSHOP}\" xmlns:Iptc4xmpCore=\"{IPTC_CORE}\" xmlns:xmpRights=\"{XMP_RIGHTS}\""
    )
}

/// What other applications keep inside the sidecar's creator contact
/// structure (properties Rawtonic does not model), for the structure
/// Rawtonic writes in its place: the start tag's namespace declarations and
/// attributes, and the child elements, copied as they are. The declarations
/// are those in scope at the old structure, so the copies mean the same.
#[derive(Default)]
struct ForeignContact {
    start: String,
    children: String,
}

/// Prefixes the description Rawtonic writes declares (`merge_xmp_with`).
const OWN_PREFIXES: [(&str, &str); 8] = [
    ("rdf", RDF),
    ("xmp", XMP),
    ("dc", DC),
    ("lr", LR),
    ("rawtonic", RT),
    ("photoshop", PHOTOSHOP),
    ("Iptc4xmpCore", IPTC_CORE),
    ("xmpRights", XMP_RIGHTS),
];

fn foreign_contact(text: &str, root: roxmltree::Node) -> Result<ForeignContact> {
    let Some(node) = root
        .descendants()
        .find(|n| n.has_tag_name((IPTC_CORE, "CreatorContactInfo")))
    else {
        return Ok(ForeignContact::default());
    };
    // The structure as `rdf:parseType="Resource"` or a nested description.
    let inner = node
        .children()
        .find(|n| n.has_tag_name((RDF, "Description")));
    let mut start = String::new();
    let mut children = String::new();
    for holder in std::iter::once(node).chain(inner) {
        for attr in holder.attributes() {
            if attr.namespace() != Some(RDF) && contact_key(attr.namespace(), attr.name()).is_none()
            {
                start.push(' ');
                start.push_str(&text[attr.range()]);
            }
        }
    }
    for child in inner.unwrap_or(node).children().filter(|n| n.is_element()) {
        let tag = child.tag_name();
        if contact_key(tag.namespace(), tag.name()).is_none() {
            children.push_str(&text[child.range()]);
        }
    }
    if start.is_empty() && children.is_empty() {
        return Ok(ForeignContact::default());
    }
    // Copies keep their meaning under the declarations in scope where the
    // children are (the nested description's, when there is one); an outer
    // attribute whose prefix the nested description redeclares would not.
    let scope = inner.unwrap_or(node);
    if let Some(inner) = inner {
        for attr in node.attributes() {
            let Some(prefix) = text[attr.range_qname()].split_once(':').map(|(p, _)| p) else {
                continue;
            };
            ensure!(
                inner.lookup_namespace_uri(Some(prefix)) == attr.namespace(),
                "XMP prefix {prefix} is redeclared in the creator contact; refusing to rewrite it"
            );
        }
    }
    let mut declarations = String::new();
    for ns in scope.namespaces() {
        match ns.name() {
            Some("xml") => {}
            Some(prefix) => match OWN_PREFIXES.iter().find(|(own, _)| *own == prefix) {
                Some((_, uri)) => ensure!(
                    *uri == ns.uri(),
                    "XMP prefix {prefix} names another namespace; refusing to rewrite the creator contact"
                ),
                None => declarations.push_str(&format!(" xmlns:{prefix}=\"{}\"", escape(ns.uri()))),
            },
            None => declarations.push_str(&format!(" xmlns=\"{}\"", escape(ns.uri()))),
        }
    }
    Ok(ForeignContact {
        start: declarations + &start,
        children,
    })
}

/// The IPTC fields as XMP elements (prefixes declared by the caller).
fn iptc_elements(
    metadata: &Metadata,
    kept_contact: &BTreeMap<String, String>,
    foreign: &ForeignContact,
) -> String {
    let prefix = |ns: &str| match ns {
        PHOTOSHOP => "photoshop",
        IPTC_CORE => "Iptc4xmpCore",
        _ => "xmpRights",
    };
    let mut out = String::new();
    let mut contact = String::new();
    for (key, ns, name, kind) in IPTC_FIELDS {
        let value = metadata.iptc.get(*key).or_else(|| {
            (*kind == IptcKind::Contact)
                .then(|| kept_contact.get(*key))
                .flatten()
        });
        let Some(value) = value.filter(|v| !v.trim().is_empty()) else {
            continue;
        };
        let tag = format!("{}:{name}", prefix(ns));
        match kind {
            IptcKind::Simple => out.push_str(&format!("<{tag}>{}</{tag}>", escape(value))),
            IptcKind::Alt => out.push_str(&format!(
                "<{tag}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></{tag}>",
                escape(value)
            )),
            IptcKind::Contact => contact.push_str(&format!("<{tag}>{}</{tag}>", escape(value))),
        }
    }
    if !contact.is_empty() {
        out.push_str(&format!(
            "<Iptc4xmpCore:CreatorContactInfo{} rdf:parseType=\"Resource\">{contact}{}</Iptc4xmpCore:CreatorContactInfo>",
            foreign.start, foreign.children
        ));
    }
    out
}

/// Update only the namespaces/properties owned by this model; Adobe develop,
/// maker namespaces and unmodeled IPTC remain byte-for-byte untouched.
pub fn merge_xmp(
    existing: Option<&str>,
    metadata: &Metadata,
    recipe: Option<&serde_json::Value>,
) -> Result<String> {
    merge_xmp_with(existing, metadata, recipe, false)
}

/// `merge_xmp`; with `replace_iptc` the IPTC fields Rawtonic models are
/// the library's only (one the library has empty is removed from the
/// sidecar), for resolving a conflict in the library's favour. Without it,
/// a field the library does not hold keeps the sidecar's value. Either way
/// a library with creator contact fields rewrites the whole contact
/// structure, as before.
fn merge_xmp_with(
    existing: Option<&str>,
    metadata: &Metadata,
    recipe: Option<&serde_json::Value>,
    replace_iptc: bool,
) -> Result<String> {
    metadata.validate()?;
    let base = existing.unwrap_or("<?xml version=\"1.0\" encoding=\"UTF-8\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"></rdf:RDF></x:xmpmeta>");
    ensure!(base.len() <= 16 * 1024 * 1024, "XMP exceeds 16 MiB limit");
    let doc = roxmltree::Document::parse(base)?;
    let root = doc
        .descendants()
        .find(|n| n.has_tag_name((RDF, "RDF")))
        .context("Missing rdf:RDF element")?;
    let root_text = &base[root.range()];
    ensure!(
        !root_text.trim_end().ends_with("/>"),
        "Self-closing RDF root needs expansion before editing"
    );
    let close = root.range().start + root_text.rfind("</").context("Missing RDF closing tag")?;
    let mut removals: Vec<std::ops::Range<usize>> = Vec::new();
    // IPTC fields this model has a value for replace the sidecar's; the
    // others stay as they are, so IPTC written by Lightroom or Capture One
    // before Rawtonic knew the field is never dropped.
    let has_contact = IPTC_FIELDS
        .iter()
        .any(|(k, _, _, kind)| *kind == IptcKind::Contact && metadata.iptc.contains_key(*k));
    // With contact fields the structure is rewritten as a whole (below);
    // replacing without any, only its modelled fields go, so properties of
    // other applications inside it stay.
    let managed = |namespace: Option<&str>, name: &str| {
        managed(namespace, name)
            || iptc_key(namespace, name)
                .is_some_and(|k| replace_iptc || metadata.iptc.contains_key(k))
            || (has_contact && namespace == Some(IPTC_CORE) && name == "CreatorContactInfo")
            || (replace_iptc && !has_contact && contact_key(namespace, name).is_some())
    };
    let owned =
        |node: roxmltree::Node| managed(node.tag_name().namespace(), node.tag_name().name());
    // A description left with nothing but our removed properties goes as a
    // whole; otherwise every rewrite left an empty one behind and the sidecar
    // grew with each save. Foreign properties keep their description.
    let emptied = |node: roxmltree::Node| {
        node.has_tag_name((RDF, "Description"))
            && node.attributes().all(|attr| {
                managed(attr.namespace(), attr.name())
                    || (attr.namespace() == Some(RDF) && attr.name() == "about")
            })
            && node.children().all(|child| {
                if child.is_element() {
                    owned(child)
                } else {
                    child.text().is_none_or(|text| text.trim().is_empty())
                }
            })
    };
    let removed = |node: roxmltree::Node| owned(node) || emptied(node);
    for node in root.descendants().filter(|n| n.is_element() && *n != root) {
        // Only record the outer removed subtree.
        if node
            .ancestors()
            .skip(1)
            .any(|n| n.is_element() && removed(n))
        {
            continue;
        }
        if removed(node) {
            removals.push(node.range());
            continue;
        }
        for attr in node.attributes() {
            if managed(attr.namespace(), attr.name()) {
                removals.push(attr.range());
            }
        }
    }
    removals.sort_by_key(|r| r.start);
    let mut out = String::new();
    let mut cursor = 0;
    for range in removals {
        if range.start >= cursor {
            out.push_str(&base[cursor..range.start]);
            cursor = range.end;
        }
    }
    out.push_str(&base[cursor..close]);
    // Line breaks around the replaced description would otherwise pile up.
    out.truncate(out.trim_end().len());
    out.push_str(&format!("\n<rdf:Description xmlns:rdf=\"{RDF}\" xmlns:xmp=\"{XMP}\" xmlns:dc=\"{DC}\" xmlns:lr=\"{LR}\" xmlns:rawtonic=\"{RT}\"{} rdf:about=\"\" xmp:Rating=\"{}\" xmp:Label=\"{}\" rawtonic:Flag=\"{}\">", iptc_namespaces(metadata), metadata.rating, escape(&metadata.label), metadata.flag));
    for (tag, val) in [
        ("title", &metadata.title),
        ("description", &metadata.description),
        ("rights", &metadata.copyright),
    ] {
        out.push_str(&format!(
            "<dc:{tag}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:{tag}>",
            escape(val)
        ));
    }
    out.push_str(&format!(
        "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>",
        escape(&metadata.creator)
    ));
    // The contact structure is rewritten as a whole; its fields this model
    // does not set keep the sidecar's values.
    let kept_contact: BTreeMap<String, String> = if has_contact && !replace_iptc {
        read_xmp(base)
            .map(|(old, _)| old.iptc)
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| iptc_field(k).is_some_and(|(_, _, kind)| kind == IptcKind::Contact))
            .collect()
    } else {
        BTreeMap::new()
    };
    // The structure is rewritten (`has_contact`): what other applications
    // keep in it goes along.
    let foreign = if has_contact {
        foreign_contact(base, root)?
    } else {
        ForeignContact::default()
    };
    out.push_str(&iptc_elements(metadata, &kept_contact, &foreign));
    for tag in ["dc:subject", "lr:hierarchicalSubject"] {
        out.push_str(&format!("<{tag}><rdf:Bag>"));
        for keyword in &metadata.keywords {
            out.push_str(&format!("<rdf:li>{}</rdf:li>", escape(keyword)));
        }
        out.push_str(&format!("</rdf:Bag></{tag}>"));
    }
    if let Some(recipe) = recipe {
        out.push_str(&format!(
            "<rawtonic:Recipe>{}</rawtonic:Recipe>",
            escape(&serde_json::to_string(recipe)?)
        ));
    }
    out.push_str("</rdf:Description>\n");
    out.push_str(&base[close..]);
    // Refuse to persist if our preservation logic produced malformed XML.
    read_xmp(&out)?;
    Ok(out)
}

/// The XMP packet embedded in exported images: the descriptive metadata a
/// recipient should see (rating, label, title, caption, creator, copyright,
/// keywords) and the creating tool. Culling flags and the recipe stay in the
/// session. Keywords go to `dc:subject` as single terms, every level of a
/// hierarchical `A|B|C` keyword included, and hierarchical keywords also to
/// `lr:hierarchicalSubject`, as Lightroom exports them.
pub fn export_xmp(metadata: &Metadata, creator_tool: &str) -> Result<String> {
    export_xmp_with_source(metadata, creator_tool, None)
}

/// `export_xmp` with an IPTC Digital Source Type (`Iptc4xmpExt:DigitalSourceType`).
pub fn export_xmp_with_source(
    metadata: &Metadata,
    creator_tool: &str,
    digital_source_type: Option<&str>,
) -> Result<String> {
    metadata.validate()?;
    ensure!(
        !creator_tool.chars().any(|c| c < '\u{20}'),
        "Creator tool contains control characters"
    );
    let mut out = String::from(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">",
    );
    out.push_str(&format!(
        "<rdf:RDF xmlns:rdf=\"{RDF}\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"{XMP}\" xmlns:dc=\"{DC}\" xmlns:lr=\"{LR}\"{} xmp:CreatorTool=\"{}\"",
        iptc_namespaces(metadata),
        escape(creator_tool)
    ));
    if let Some(source) = digital_source_type {
        out.push_str(&format!(
            " xmlns:Iptc4xmpExt=\"{IPTC_EXT}\" Iptc4xmpExt:DigitalSourceType=\"{}\"",
            escape(source)
        ));
    }
    if metadata.rating > 0 {
        out.push_str(&format!(" xmp:Rating=\"{}\"", metadata.rating));
    }
    if !metadata.label.is_empty() {
        out.push_str(&format!(" xmp:Label=\"{}\"", escape(&metadata.label)));
    }
    out.push('>');
    for (tag, value) in [
        ("title", &metadata.title),
        ("description", &metadata.description),
        ("rights", &metadata.copyright),
    ] {
        if !value.trim().is_empty() {
            out.push_str(&format!(
                "<dc:{tag}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:{tag}>",
                escape(value)
            ));
        }
    }
    if !metadata.creator.trim().is_empty() {
        out.push_str(&format!(
            "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>",
            escape(&metadata.creator)
        ));
    }
    out.push_str(&iptc_elements(
        metadata,
        &BTreeMap::new(),
        &ForeignContact::default(),
    ));
    let mut terms: Vec<&str> = Vec::new();
    for term in metadata
        .keywords
        .iter()
        .flat_map(|k| k.split('|'))
        .map(str::trim)
    {
        if !term.is_empty() && !terms.contains(&term) {
            terms.push(term);
        }
    }
    let hierarchical: Vec<&String> = metadata
        .keywords
        .iter()
        .filter(|k| k.contains('|'))
        .collect();
    for (tag, values) in [
        (
            "dc:subject",
            terms.iter().map(|t| escape(t)).collect::<Vec<_>>(),
        ),
        (
            "lr:hierarchicalSubject",
            hierarchical.iter().map(|k| escape(k)).collect(),
        ),
    ] {
        if !values.is_empty() {
            out.push_str(&format!("<{tag}><rdf:Bag>"));
            for value in values {
                out.push_str(&format!("<rdf:li>{value}</rdf:li>"));
            }
            out.push_str(&format!("</rdf:Bag></{tag}>"));
        }
    }
    out.push_str("</rdf:Description></rdf:RDF></x:xmpmeta>\n<?xpacket end=\"w\"?>");
    roxmltree::Document::parse(&out)?;
    Ok(out)
}

/// The sidecar changed since the library last wrote or read it (another
/// application edited, replaced or removed it): `write_xmp` refuses to
/// replace it until the change is reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XmpConflict;

impl std::fmt::Display for XmpConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("XMP conflict: external changes must be reviewed")
    }
}

impl std::error::Error for XmpConflict {}

/// expected_hash is the DB's last observed digest. Conflicts never overwrite
/// (`XmpConflict`), and neither is a read-only sidecar replaced: a locked
/// file stays as it is and the write fails.
pub fn write_xmp(
    path: &Path,
    metadata: &Metadata,
    recipe: Option<&serde_json::Value>,
    expected_hash: Option<&str>,
) -> Result<String> {
    write_xmp_with(path, metadata, recipe, expected_hash, false)
}

/// `write_xmp` for a conflict resolved in the library's favour: the IPTC
/// fields Rawtonic models are written as the library holds them, an empty
/// one removing the sidecar's (`merge_xmp_with`). Properties Rawtonic does
/// not model are kept as with `write_xmp`.
pub fn write_xmp_replacing_iptc(
    path: &Path,
    metadata: &Metadata,
    recipe: Option<&serde_json::Value>,
    expected_hash: Option<&str>,
) -> Result<String> {
    write_xmp_with(path, metadata, recipe, expected_hash, true)
}

fn write_xmp_with(
    path: &Path,
    metadata: &Metadata,
    recipe: Option<&serde_json::Value>,
    expected_hash: Option<&str>,
    replace_iptc: bool,
) -> Result<String> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    if let Some(expected) = expected_hash
        && !existing
            .as_ref()
            .is_some_and(|s| digest(s.as_bytes()) == expected)
    {
        return Err(XmpConflict.into());
    }
    if existing.is_some() {
        ensure!(
            !fs::metadata(path)?.permissions().readonly(),
            "The XMP sidecar is read-only"
        );
    }
    let merged = merge_xmp_with(existing.as_deref(), metadata, recipe, replace_iptc)?;
    atomic_write(path, merged.as_bytes())?;
    Ok(digest(merged.as_bytes()))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn a_replaced_file_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::atomic_write(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    use super::*;
    #[test]
    fn a_changed_sidecar_is_a_typed_conflict_and_stays_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.xmp");
        let hash = write_xmp(&path, &Metadata::default(), None, None).unwrap();
        std::fs::write(&path, "changed elsewhere").unwrap();
        let error = write_xmp(&path, &Metadata::default(), None, Some(&hash)).unwrap_err();
        assert!(error.is::<XmpConflict>(), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "changed elsewhere");
    }

    #[test]
    fn a_packet_carries_a_flag_only_with_rawtonics_property_or_a_rejection() {
        let packet = |attrs: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF}"><rdf:Description xmlns:xmp="{XMP}" xmlns:rawtonic="{RT}" {attrs}/></rdf:RDF></x:xmpmeta>"#
            )
        };
        assert!(!carries_flag(&packet(r#"xmp:Rating="3""#)).unwrap());
        assert!(carries_flag(&packet(r#"xmp:Rating="-1""#)).unwrap());
        assert!(carries_flag(&packet(r#"xmp:Rating="3" rawtonic:Flag="0""#)).unwrap());
        let written = merge_xmp(None, &Metadata::default(), None).unwrap();
        assert!(carries_flag(&written).unwrap());
    }

    #[test]
    fn replacing_iptc_keeps_foreign_properties_inside_the_contact_structure() {
        let packet = format!(
            r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF}"><rdf:Description rdf:about="" xmlns:Iptc4xmpCore="{IPTC_CORE}" xmlns:other="https://example.invalid/other/"><Iptc4xmpCore:CreatorContactInfo rdf:parseType="Resource"><Iptc4xmpCore:CiEmailWork>someone@example.invalid</Iptc4xmpCore:CiEmailWork><other:Pager>123</other:Pager></Iptc4xmpCore:CreatorContactInfo></rdf:Description></rdf:RDF></x:xmpmeta>"#
        );
        let replaced = merge_xmp_with(Some(&packet), &Metadata::default(), None, true).unwrap();
        assert!(!replaced.contains("someone@"), "{replaced}");
        assert!(
            replaced.contains("<other:Pager>123</other:Pager>"),
            "{replaced}"
        );
        assert!(read_xmp(&replaced).unwrap().0.iptc.is_empty());
        // Without replacing, the sidecar's contact field stays.
        let kept = merge_xmp(Some(&packet), &Metadata::default(), None).unwrap();
        assert!(kept.contains("someone@"), "{kept}");
    }

    #[test]
    fn rewriting_the_contact_structure_keeps_foreign_properties_inside_it() {
        for structure in [
            r#"<Iptc4xmpCore:CreatorContactInfo rdf:parseType="Resource" other:Note="n"><Iptc4xmpCore:CiEmailWork>someone@example.invalid</Iptc4xmpCore:CiEmailWork><other:Pager>123</other:Pager></Iptc4xmpCore:CreatorContactInfo>"#,
            r#"<Iptc4xmpCore:CreatorContactInfo><rdf:Description other:Note="n"><Iptc4xmpCore:CiEmailWork>someone@example.invalid</Iptc4xmpCore:CiEmailWork><other:Pager>123</other:Pager></rdf:Description></Iptc4xmpCore:CreatorContactInfo>"#,
        ] {
            let packet = format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF}"><rdf:Description rdf:about="" xmlns:Iptc4xmpCore="{IPTC_CORE}" xmlns:other="https://example.invalid/other/">{structure}</rdf:Description></rdf:RDF></x:xmpmeta>"#
            );
            // As Use XMP reads it, written back with the library's metadata.
            let mut meta = read_xmp(&packet).unwrap().0;
            meta.set_iptc("creatorPhone", "+1 555");
            for replace in [false, true] {
                let written = merge_xmp_with(Some(&packet), &meta, None, replace).unwrap();
                let doc = roxmltree::Document::parse(&written).unwrap();
                let pager = doc
                    .descendants()
                    .find(|n| n.has_tag_name(("https://example.invalid/other/", "Pager")))
                    .unwrap_or_else(|| panic!("{written}"));
                assert_eq!(pager.text(), Some("123"));
                assert!(
                    doc.descendants()
                        .any(|n| n.attribute(("https://example.invalid/other/", "Note"))
                            == Some("n"))
                );
                let read = read_xmp(&written).unwrap().0;
                assert_eq!(
                    read.iptc.get("creatorEmail").map(String::as_str),
                    Some("someone@example.invalid")
                );
                assert_eq!(
                    read.iptc.get("creatorPhone").map(String::as_str),
                    Some("+1 555")
                );
                assert_eq!(
                    doc.descendants()
                        .filter(|n| n.has_tag_name((IPTC_CORE, "CreatorContactInfo")))
                        .count(),
                    1,
                    "{written}"
                );
            }
        }
    }

    #[test]
    fn contact_properties_keep_the_namespaces_of_a_nested_description() {
        let packet = |inner: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF}"><rdf:Description rdf:about="" xmlns:Iptc4xmpCore="{IPTC_CORE}" xmlns:other="https://example.invalid/other/"><Iptc4xmpCore:CreatorContactInfo other:Note="n">{inner}</Iptc4xmpCore:CreatorContactInfo></rdf:Description></rdf:RDF></x:xmpmeta>"#
            )
        };
        let mut meta = Metadata::default();
        meta.set_iptc("creatorPhone", "+1 555");
        // Declared on the nested description, also shadowing an outer prefix.
        let nested = packet(
            r#"<rdf:Description xmlns:inner="https://example.invalid/inner/"><inner:Thing>t</inner:Thing><Iptc4xmpCore:CiEmailWork>someone@example.invalid</Iptc4xmpCore:CiEmailWork></rdf:Description>"#,
        );
        let written = merge_xmp(Some(&nested), &meta, None).unwrap();
        let doc = roxmltree::Document::parse(&written).unwrap();
        assert!(
            doc.descendants()
                .any(|n| n.has_tag_name(("https://example.invalid/inner/", "Thing"))),
            "{written}"
        );
        assert!(
            doc.descendants()
                .any(|n| n.attribute(("https://example.invalid/other/", "Note")) == Some("n"))
        );
        let shadowing = packet(
            r#"<rdf:Description xmlns:other="https://example.invalid/elsewhere/"><other:Thing>t</other:Thing></rdf:Description>"#,
        );
        assert!(
            merge_xmp(Some(&shadowing), &meta, None).is_err(),
            "fails closed"
        );
    }

    #[test]
    fn a_rejected_rating_wins_over_a_pick_in_any_order() {
        for attrs in [
            r#"xmp:Rating="-1" rawtonic:Flag="1""#,
            r#"rawtonic:Flag="1" xmp:Rating="-1""#,
        ] {
            let packet = format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF}"><rdf:Description xmlns:xmp="{XMP}" xmlns:rawtonic="{RT}" {attrs}/></rdf:RDF></x:xmpmeta>"#
            );
            assert_eq!(read_xmp(&packet).unwrap().0.flag, -1, "{attrs}");
        }
    }

    #[test]
    fn export_packet_can_mark_generated_areas() {
        let packet = export_xmp_with_source(
            &Metadata::default(),
            "Rawtonic",
            Some(DIGITAL_SOURCE_COMPOSITE_AI),
        )
        .unwrap();
        let doc = roxmltree::Document::parse(&packet).unwrap();
        let description = doc
            .descendants()
            .find(|n| n.has_tag_name((RDF, "Description")))
            .unwrap();
        assert_eq!(
            description.attribute((IPTC_EXT, "DigitalSourceType")),
            Some(DIGITAL_SOURCE_COMPOSITE_AI)
        );
        let plain = export_xmp(&Metadata::default(), "Rawtonic").unwrap();
        assert!(!plain.contains("DigitalSourceType"));
    }

    #[test]
    fn export_packet_carries_descriptive_metadata_only() {
        let meta = Metadata {
            rating: 4,
            flag: 1,
            label: "Green".into(),
            keywords: vec![
                "Places|Germany|Berlin".into(),
                "Berlin".into(),
                "A & B".into(),
            ],
            title: "Title <1>".into(),
            description: "Caption".into(),
            creator: "Jane Example".into(),
            copyright: "© 2026 Jane Example".into(),
            ..Default::default()
        };
        let packet = export_xmp(&meta, "Rawtonic 0.1.0").unwrap();
        assert!(packet.starts_with("<?xpacket begin=\"\u{feff}\""));
        assert!(packet.ends_with("<?xpacket end=\"w\"?>"));
        assert!(packet.contains("xmp:CreatorTool=\"Rawtonic 0.1.0\""));
        assert!(!packet.contains("Flag") && !packet.contains("Recipe"));
        assert!(packet.contains(
            "<dc:subject><rdf:Bag><rdf:li>Places</rdf:li><rdf:li>Germany</rdf:li><rdf:li>Berlin</rdf:li><rdf:li>A &amp; B</rdf:li></rdf:Bag></dc:subject>"
        ));
        assert!(packet.contains(
            "<lr:hierarchicalSubject><rdf:Bag><rdf:li>Places|Germany|Berlin</rdf:li></rdf:Bag></lr:hierarchicalSubject>"
        ));
        let (read, recipe) = read_xmp(&packet).unwrap();
        assert_eq!(recipe, None);
        assert_eq!(
            (
                read.rating,
                read.flag,
                read.label.as_str(),
                read.title.as_str()
            ),
            (4, 0, "Green", "Title <1>")
        );
        assert_eq!(
            (
                read.description.as_str(),
                read.creator.as_str(),
                read.copyright.as_str()
            ),
            ("Caption", "Jane Example", "© 2026 Jane Example")
        );
    }

    #[test]
    fn export_packet_without_session_metadata_names_only_the_tool() {
        let packet = export_xmp(&Metadata::default(), "Rawtonic 0.1.0").unwrap();
        assert!(packet.contains("xmp:CreatorTool=\"Rawtonic 0.1.0\">"));
        for absent in [
            "Rating",
            "Label",
            "dc:title",
            "dc:creator",
            "dc:subject",
            "hierarchical",
        ] {
            assert!(!packet.contains(absent), "{absent}");
        }
        assert_eq!(read_xmp(&packet).unwrap().0, Metadata::default());
    }

    #[test]
    fn roundtrip_and_preserve_foreign_namespaces() {
        let text = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmp:Rating="2" crs:Exposure2012="1.25"><crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li></rdf:Seq></crs:ToneCurvePV2012></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let meta = Metadata {
            rating: 5,
            flag: 1,
            label: "Red".into(),
            keywords: vec!["Jobs|Acme & Zürich".into()],
            title: "A < B".into(),
            ..Default::default()
        };
        let recipe = serde_json::json!({"schema_version": 1, "adjustments": {"exposure": 0.75}});
        let result = merge_xmp(Some(text), &meta, Some(&recipe)).unwrap();
        assert!(result.contains("crs:Exposure2012=\"1.25\""));
        assert!(result.contains(
            "<crs:ToneCurvePV2012><rdf:Seq><rdf:li>0, 0</rdf:li></rdf:Seq></crs:ToneCurvePV2012>"
        ));
        assert_eq!(
            read_xmp(&result).unwrap(),
            (meta.clone(), Some(recipe.clone()))
        );
        assert_eq!(
            read_xmp(&merge_xmp(Some(&result), &meta, Some(&recipe)).unwrap())
                .unwrap()
                .0,
            meta
        );
    }
    #[test]
    fn sidecars_written_as_re_touch_are_read_and_rewritten_as_rawtonic() {
        // As the app wrote sidecars before the rename, with a foreign property.
        let former = r#"<?xml version="1.0" encoding="UTF-8"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" rdf:about="" crs:Exposure2012="0.5"/>
<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:lr="http://ns.adobe.com/lightroom/1.0/" xmlns:retouch="https://retouch.photo/ns/1.0/" rdf:about="" xmp:Rating="4" xmp:Label="Green" retouch:Flag="1"><dc:subject><rdf:Bag><rdf:li>Job</rdf:li></rdf:Bag></dc:subject><retouch:Recipe>{&quot;adjustments&quot;:{&quot;exposure&quot;:0.25}}</retouch:Recipe></rdf:Description>
</rdf:RDF></x:xmpmeta>"#;
        let (meta, recipe) = read_xmp(former).unwrap();
        assert_eq!(
            (meta.rating, meta.flag, meta.label.as_str()),
            (4, 1, "Green")
        );
        assert_eq!(meta.keywords, vec!["Job".to_string()]);
        let recipe = recipe.expect("the former recipe is read");
        assert_eq!(
            recipe,
            serde_json::json!({"adjustments": {"exposure": 0.25}})
        );

        let rewritten = merge_xmp(Some(former), &meta, Some(&recipe)).unwrap();
        assert!(
            !rewritten.contains(FORMER_RT),
            "the former namespace is replaced"
        );
        assert!(!rewritten.contains("retouch:"));
        assert_eq!(rewritten.matches("rawtonic:Flag=").count(), 1);
        assert_eq!(rewritten.matches("<rawtonic:Recipe>").count(), 1);
        assert!(
            rewritten.contains("crs:Exposure2012=\"0.5\""),
            "foreign properties stay"
        );
        assert_eq!(
            read_xmp(&rewritten).unwrap(),
            (meta.clone(), Some(recipe.clone()))
        );
        // Rewriting again leaves exactly one description of ours: the
        // sidecar no longer grows with every save.
        let again = merge_xmp(Some(&rewritten), &meta, Some(&recipe)).unwrap();
        assert_eq!(again, rewritten);
        assert_eq!(again.matches("<rdf:Description").count(), 2);
    }
    #[test]
    fn conflict_and_malformed_input_leave_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.xmp");
        let hash = write_xmp(&p, &Metadata::default(), None, None).unwrap();
        fs::write(&p, "external").unwrap();
        assert!(write_xmp(&p, &Metadata::default(), None, Some(&hash)).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "external");
        assert!(write_xmp(&p, &Metadata::default(), None, None).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_target_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("a.arw");
        let sidecar = dir.path().join("a.xmp");
        fs::write(&original, b"sacred").unwrap();
        std::os::unix::fs::symlink(&original, &sidecar).unwrap();
        assert!(atomic_write(&sidecar, "changed").is_err());
        assert_eq!(fs::read(original).unwrap(), b"sacred");
    }

    /// An IPTC block as Lightroom writes it (attributes, an Alt, the contact
    /// structure as a resource).
    const LIGHTROOM_IPTC: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/" xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/" xmlns:xmpRights="http://ns.adobe.com/xap/1.0/rights/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
  photoshop:Headline="Summer at the lake" photoshop:City="Ascona" photoshop:Country="Switzerland"
  Iptc4xmpCore:CountryCode="CH" Iptc4xmpCore:Location="Lido" photoshop:Credit="Alex Example" crs:Exposure2012="+0.50">
 <xmpRights:UsageTerms><rdf:Alt><rdf:li xml:lang="x-default">Editorial use only</rdf:li></rdf:Alt></xmpRights:UsageTerms>
 <Iptc4xmpCore:CreatorContactInfo rdf:parseType="Resource"><Iptc4xmpCore:CiEmailWork>n@example.org</Iptc4xmpCore:CiEmailWork><Iptc4xmpCore:CiAdrCity>Zürich</Iptc4xmpCore:CiAdrCity></Iptc4xmpCore:CreatorContactInfo>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;

    #[test]
    fn iptc_core_is_read_from_lightroom_style_xmp() {
        let (meta, _) = read_xmp(LIGHTROOM_IPTC).unwrap();
        let get = |k: &str| meta.iptc.get(k).map(String::as_str);
        assert_eq!(get("headline"), Some("Summer at the lake"));
        assert_eq!(get("city"), Some("Ascona"));
        assert_eq!(get("countryCode"), Some("CH"));
        assert_eq!(get("location"), Some("Lido"));
        assert_eq!(get("usageTerms"), Some("Editorial use only"));
        assert_eq!(get("creatorEmail"), Some("n@example.org"));
        assert_eq!(get("creatorCity"), Some("Zürich"));
        assert_eq!(get("state"), None);
        // Contact fields only count inside the contact structure.
        let stray = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:Iptc4xmpCore="http://iptc.org/std/Iptc4xmpCore/1.0/xmlns/" Iptc4xmpCore:CiEmailWork="x@y"/></rdf:RDF></x:xmpmeta>"#;
        assert!(read_xmp(stray).unwrap().0.iptc.is_empty());
    }

    #[test]
    fn merging_replaces_set_iptc_fields_and_keeps_the_others() {
        let mut meta = read_xmp(LIGHTROOM_IPTC).unwrap().0;
        meta.iptc.clear();
        meta.set_iptc("city", "Locarno");
        meta.set_iptc("state", "Ticino");
        let merged = merge_xmp(Some(LIGHTROOM_IPTC), &meta, None).unwrap();
        let (back, _) = read_xmp(&merged).unwrap();
        let get = |k: &str| back.iptc.get(k).map(String::as_str);
        assert_eq!(get("city"), Some("Locarno"), "replaced");
        assert_eq!(get("state"), Some("Ticino"), "added");
        assert_eq!(get("headline"), Some("Summer at the lake"), "kept");
        assert_eq!(get("usageTerms"), Some("Editorial use only"), "kept");
        assert_eq!(get("creatorEmail"), Some("n@example.org"), "kept");
        assert!(
            merged.contains("crs:Exposure2012=\"+0.50\""),
            "develop settings kept"
        );
        assert_eq!(merged.matches("<photoshop:City>").count(), 1, "{merged}");
        assert!(
            !merged.contains("photoshop:City=\""),
            "the old attribute went"
        );
        // Setting a contact field rewrites the contact structure.
        meta.set_iptc("creatorPhone", "+41 00");
        let merged = merge_xmp(Some(&merged), &meta, None).unwrap();
        let back = read_xmp(&merged).unwrap().0;
        assert_eq!(
            back.iptc.get("creatorPhone").map(String::as_str),
            Some("+41 00")
        );
        assert_eq!(
            back.iptc.get("creatorEmail").map(String::as_str),
            Some("n@example.org"),
            "kept in the structure"
        );
        assert_eq!(
            merged.matches("<Iptc4xmpCore:CreatorContactInfo").count(),
            1,
            "one structure: {merged}"
        );
    }

    #[test]
    fn exports_carry_iptc_and_leave_photos_without_it_unchanged() {
        let mut meta = Metadata {
            title: "T".into(),
            ..Default::default()
        };
        let plain = export_xmp(&meta, "Rawtonic").unwrap();
        assert!(!plain.contains("photoshop"), "{plain}");
        meta.set_iptc("headline", "H & co");
        meta.set_iptc("creatorUrl", "https://example.org");
        let packet = export_xmp(&meta, "Rawtonic").unwrap();
        assert!(packet.contains("<photoshop:Headline>H &amp; co</photoshop:Headline>"));
        let back = read_xmp(&packet).unwrap().0;
        assert_eq!(
            back.iptc.get("creatorUrl").map(String::as_str),
            Some("https://example.org")
        );
        let mut bad = Metadata::default();
        bad.iptc.insert("nonsense".into(), "x".into());
        assert!(bad.validate().is_err());
    }
}
