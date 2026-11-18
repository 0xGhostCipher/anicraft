//! Structural decode of a legacy document: scenes, layers and library names into the
//! AniCraft document model.
//!
//! Observed format (Flash 8–CS4 era; see ADR 0096 and the corpus notes): every stream
//! starts with a 5-byte prologue, and its records are fields of `ff fe ff`- or
//! `01 80 17`-prefixed strings (UTF-16, a one-byte length) and raw numerics. The
//! `Contents` stream lists the document: library items (stream name + label pairs) and
//! one `CDocumentPage` record per scene (page stream name + scene label). A page stream
//! (`P <n> <stamp>`) holds its scene's timeline: one record per layer, whose name trails
//! the layer's data behind a constant `00 80 00 00 00 80 00 00 0b` anchor — layers are
//! stored bottom-first, so the decode reverses them for AniCraft's top-first timelines.
//!
//! This pass decodes the *structure* only. Frame records, artwork, tweens, text, bitmaps
//! and sounds stay for the record-level decode pass and are counted in the report, so a
//! file opens with its scenes, layers and library rather than failing outright.

use crate::cfb::Compound;
use crate::records;
use crate::shape;
use crate::report::{ImportError, Report};
use anicraft_doc::{Document, Element, ElementId, ElementKind, Keyframe, Layer, LayerId, Scene, Stage, Symbol, SymbolKind, SymbolId, Timeline};
use std::sync::Arc;

/// The `Contents` stream's version byte on every corpus file of the Flash 8–CS4 era.
const CONTENTS_VERSION: u8 = 0x3F;

/// The 3-byte prefix of a stream-name string (`01 80 17`), then a one-byte length.
const STREAM_NAME_PREFIX: [u8; 2] = [0x01, 0x80];
/// The 3-byte marker before a named record's UTF-16 string (`ff fe ff`), then a length byte.
const STRING_MARKER: [u8; 3] = [0xFF, 0xFE, 0xFF];
/// The 9 bytes that introduce every layer record's trailing name.
const LAYER_ANCHOR: [u8; 9] = [0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x0B];
/// Longest name read (layer, scene and library labels).
const MAX_NAME: usize = 64;

/// A scene as the document lists it: its page stream's name and the scene's label.
struct Page {
    stream: String,
    label: String,
}

/// Imports a legacy `.fla`'s structure into the AniCraft model.
pub fn import(data: &[u8]) -> Result<(Document, Report), ImportError> {
    let file = Compound::new(data)?;
    let contents = file.stream("Contents")?;
    if contents.first() != Some(&CONTENTS_VERSION) {
        return Err(ImportError::UnsupportedVersion);
    }
    let mut report = Report::default();
    let pages = pages(&contents)?;
    let library = library(&contents, &mut report);
    if !library.is_empty() {
        report.keep("library names");
    }

    let mut doc = Document::default();
    doc.scenes.clear();
    doc.stage = Stage::default();
    // v1: frame records, artwork, tweens, text and media stay for the decode pass; the
    // document opens with its scenes, layers and library names.
    report.notes.push("the era's frame records, artwork, tweens, text and media are decoded in a later pass; the document opens with its scenes, layers and library".into());
    for page in &pages {
        let mut timeline = Timeline::default();
        if let Ok(bytes) = file.stream(&page.stream) {
            // Layer records are stored bottom-first; AniCraft's timelines are top-first.
            for name in layer_names(&bytes).into_iter().rev() {
                let id = LayerId(doc.alloc_id());
                timeline.layers.push(Arc::new(Layer::new(id, name)));
                report.keep("layers");
            }
        } else {
            report.skip_named("scenes whose page stream is missing", page.label.clone());
        }
        let id = LayerId(doc.alloc_id());
        if timeline.layers.is_empty() {
            timeline.layers.push(Arc::new(Layer::new(id, "Layer 1")));
        }
        doc.scenes.push(Arc::new(Scene { name: page.label.clone(), timeline, guides: Vec::new() }));
    }
    if doc.scenes.is_empty() {
        let timeline = Timeline { layers: vec![Arc::new(Layer::new(LayerId(doc.alloc_id()), "Layer 1"))] };
        doc.scenes.push(Arc::new(Scene { name: "Scene 1".into(), timeline, guides: Vec::new() }));
        report.skip("scenes (no readable page record)");
    }
    // The library: each item stream is a symbol whose timeline holds its first shape
    // record's artwork (the frame-level decode will place the rest).
    let mut decoded_shapes = 0usize;
    for (stream, label) in &library {
        let Ok(bytes) = file.stream(stream) else { continue };
        let shapes = shapes(&bytes, &mut report);
        if shapes.is_empty() {
            continue;
        }
        let mut timeline = Timeline::default();
        let layer = Layer::new(LayerId(doc.alloc_id()), "artwork");
        let keyframe = Keyframe { elements: shapes.into_iter().map(Arc::new).collect(), ..Keyframe::new(0, 1) };
        let id = SymbolId(doc.alloc_id());
        timeline.layers.push(Arc::new(Layer { keyframes: vec![Arc::new(keyframe)], ..layer }));
        doc.library.push(Arc::new(Symbol {
            id,
            name: label.clone(),
            kind: SymbolKind::Graphic,
            linkage: None,
            scale_grid: None,
            scale_grid_kept: None,
            timeline,
        }));
        decoded_shapes += 1;
        report.keep("library symbols with decoded shapes");
    }
    doc.sanitize();
    count_pending_records(&file, decoded_shapes, &mut report);
    Ok((doc, report))
}

/// Decodes a library stream's first `CPicShape` record (the object that names the class)
/// into shape elements: one per fill style, then one per stroke style, carried by the
/// record's matrix. The record grammar — straight and quadratic edges, styles, gradients —
/// is in [`crate::shape`]. A record that doesn't decode is left out whole and counted; the
/// stream's other shape records wait for the frame-level decode that places them.
fn shapes(bytes: &[u8], report: &mut Report) -> Vec<Element> {
    let Some(name) = find(bytes, b"CPicShape") else { return Vec::new() };
    let after = name + b"CPicShape".len();
    let decoded = match shape::body_after_tag(bytes, after) {
        Some(body) => shape::decode(bytes, body),
        // A shape object that holds its artwork in a nested object right behind its own
        // first field (`04 00`, then the nested object's class tag; seen on a few grouped
        // shapes): that object's body.
        None => match (after..after + 16).find_map(|start| shape::body_after_tag(bytes, start).and_then(|body| shape::decode(bytes, body).ok())) {
            Some(record) => Ok(record),
            None => {
                report.skip("shapes (edge header not recognised yet)");
                return Vec::new();
            }
        },
    };
    let record = match decoded {
        Ok(record) => record,
        Err(shape::ShapeError::FillKind(0x40..=0x43)) => {
            report.skip("shapes with bitmap fills (decode pending)");
            return Vec::new();
        }
        Err(shape::ShapeError::FillKind(_)) => {
            report.skip("shapes with fills of an unknown kind");
            return Vec::new();
        }
        Err(_) => {
            report.skip("shapes whose edge records didn't decode");
            return Vec::new();
        }
    };
    for what in &record.approximations {
        report.skip(what);
    }
    let transform = record.matrix;
    record.shapes().into_iter().enumerate().map(|(i, s)| Element {
        id: ElementId(i as u64 + 1),
        name: String::new(),
        transform,
        color: anicraft_doc::ColorEffect::default(),
        kind: ElementKind::Shape(s),
    }).collect()
}

/// The document's scenes, from the `CDocumentPage` record(s): each holds the page's
/// stream name (`P 2 1522499960`, NUL-terminated UTF-16) then the scene's label behind
/// the `ff fe ff` marker.
fn pages(contents: &[u8]) -> Result<Vec<Page>, ImportError> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(at) = find(&contents[from..], b"CDocumentPage") {
        let at = from + at;
        let Some(name_end) = at.checked_add(b"CDocumentPage".len()) else { break };
        // The page record: a `17` marker and a byte of attributes, then the page's
        // stream name (UTF-16, NUL-terminated), then the scene's label behind the
        // `ff fe ff` marker.
        let rest = contents.get(name_end..).ok_or(ImportError::BadStructure("the file ends inside a page record"))?;
        if rest.first() != Some(&0x17) {
            from = name_end;
            continue;
        }
        let mut chars = Vec::new();
        let mut i = 2usize; // past the `17` marker byte and its attribute byte
        while i + 1 < rest.len() {
            if i + STRING_MARKER.len() <= rest.len() && rest[i..i + STRING_MARKER.len()] == STRING_MARKER {
                break; // the name ends at the next record's marker (no terminator)
            }
            if chars.len() > MAX_NAME {
                return Err(ImportError::BadStructure("a page record's stream name is too long"));
            }
            let c = u16::from_le_bytes([rest[i], rest[i + 1]]);
            if c == 0 {
                break;
            }
            chars.push(c);
            i += 2;
        }
        let stream = String::from_utf16(&chars).map_err(|_| ImportError::BadStructure("a page record's stream name is not text"))?;
        if stream.is_empty() || !stream.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
            return Err(ImportError::BadStructure("a page record's stream name is not text"));
        }
        // The scene label: the marker string right after the stream name.
        let after_name = i;
        let label = rest.get(after_name..)
            .and_then(|tail| (0..tail.len().saturating_sub(4)).find_map(|j| after_marker(tail, j)))
            .ok_or(ImportError::BadStructure("a page record has no scene name"))?;
        out.push(Page { stream, label: label.clone() });
        from = name_end + after_name + 4 + 2 * label.len();
    }
    Ok(out)
}

/// The library: stream-name strings (`01 80 17` prefix) each followed by a label string
/// (`ff fe ff`), e.g. `Symbol 125` → `UFO rays`, as (stream, label) pairs.
pub fn library(contents: &[u8], report: &mut Report) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + STREAM_NAME_PREFIX.len() + 2 <= contents.len() {
        if contents[i] == STREAM_NAME_PREFIX[0] && contents[i + 1] == STREAM_NAME_PREFIX[1] && contents[i + 2] == 0x17
            && let Some(name) = marked_string(contents, i + 2, 0x17) {
                // The label follows within the same record: take the first string ahead.
                let from = i + 4 + 2 * name.len();
                let to = (from + 256).min(contents.len());
                for j in from..to {
                    if let Some(label) = after_marker(contents, j) {
                        report.keep("library names");
                        i = j + 4 + 2 * label.len();
                        out.push((name, label));
                        break;
                    }
                }
                continue;
            }
        i += 1;
    }
    out
}

/// Layer names in a page stream: the constant layer anchor, the `ff fe ff` marker, then
/// the name. Other named records (a sound's file name inside a frame, for one) share the
/// anchor; they are told apart only by the record-level decode pass, so a document with
/// sounds may gain a layer named after the sound.
fn layer_names(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + LAYER_ANCHOR.len() + 4 <= bytes.len() {
        if bytes[i..i + LAYER_ANCHOR.len()] == LAYER_ANCHOR {
            // The marker's last byte sits two past the anchor's end.
            if let Some(name) = after_marker(bytes, i + LAYER_ANCHOR.len() + 2) {
                out.push(name);
                i += LAYER_ANCHOR.len() + 2 + 4 + 2 * out.last().map_or(0, |s: &String| s.len());
                continue;
            }
        }
        i += 1;
    }
    out
}

/// A string behind the `ff fe ff` marker at `at`: a one-byte length, then UTF-16.
/// `at` points at the marker's first byte; returns the text when it is plausible.
fn after_marker(data: &[u8], at: usize) -> Option<String> {
    marked_string(data, at, 0xFF)
}

/// A string whose 3rd marker byte is `third` (`ff fe ff` or `01 80 17`): `data[at]` is
/// the third marker byte, `data[at + 1]` the length, then UTF-16.
fn marked_string(data: &[u8], at: usize, third: u8) -> Option<String> {
    if data.get(at) != Some(&third) {
        return None;
    }
    let len = *data.get(at + 1)? as usize;
    if len == 0 || len > MAX_NAME || at + 2 + 2 * len > data.len() {
        return None;
    }
    let chars: Vec<u16> = data[at + 2..at + 2 + 2 * len].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let text = String::from_utf16(&chars).ok()?;
    (text.chars().all(|c| !c.is_control())).then_some(text)
}

/// Counts the record kinds the later decode pass will handle, so the report says what a
/// structure-only open left out.
fn count_pending_records(file: &Compound, mut decoded_shapes: usize, report: &mut Report) {
    for entry in file.entries() {
        if entry.kind != crate::cfb::Kind::Stream || entry.path == "Contents" {
            continue;
        }
        let Ok(bytes) = file.stream(&entry.path) else { continue };
        for tag in records::scan_tags(&bytes) {
            if tag.name == "CPicShape" && decoded_shapes > 0 {
                decoded_shapes -= 1; // decoded into a library symbol above
                continue;
            }
            let what = match tag.name.as_str() {
                "CPicShape" | "CMorphSegment" => "shapes (artwork decode pending)",
                "CPicSymbol" => "symbol placements (decode pending)",
                "CPicText" => "texts (decode pending)",
                "CPicButton" => "buttons (decode pending)",
                "CPicMorphShape" => "shape tweens (decode pending)",
                "CMediaBits" | "CPicBitmap" => "bitmaps (decode pending)",
                "CMediaSound" => "sounds (decode pending)",
                "CPicScript" => "frame scripts (decode pending)",
                _ => continue,
            };
            report.skip(what);
        }
    }
}

/// Finds `needle` in `data` (small, bounded scans; the streams are capped by the reader).
fn find(data: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || data.len() < needle.len() {
        return None;
    }
    (0..=data.len() - needle.len()).find(|&i| &data[i..i + needle.len()] == needle)
}
