//! The mapping's guards on hostile input (ADR 0096): groups used more than once or inside
//! themselves, and the `Contents` scans. Every byte here is written from the grammar in
//! `crate::archive` and `crate::doc`, never taken from a corpus file.

use crate::tests::{one_frame_stream, rectangle_body, scene, synthetic_contents, write, Ar, FrameSpec};
use anicraft_doc::{Element, ElementKind};
use std::sync::Arc;

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn frame_spec(duration: u16) -> FrameSpec {
    let mut spec = FrameSpec::default();
    spec.duration = duration;
    spec
}

/// A document of the synthetic `Contents`, the scene page and the symbol its instances place.
fn open(page: Vec<u8>) -> (anicraft_doc::Document, crate::Report) {
    let payloads = vec![("Contents", synthetic_contents()), ("P 1 1", page), ("Symbol 9", one_frame_stream(&rectangle_body()))];
    crate::doc::import(&write(&payloads)).expect("opens")
}

/// A page of one layer, "art", whose one keyframe holds what `objects` writes.
fn page_with(objects: impl FnOnce(&mut Ar)) -> Vec<u8> {
    let mut w = Ar::new();
    w.page(|w| {
        w.layer("art", 0, None, |w| w.frame(&[], &frame_spec(4), objects));
    });
    w.out
}

fn skipped(report: &crate::Report, what: &str) -> usize {
    report.skipped.get(what).copied().unwrap_or(0)
}

fn kinds(elements: &[Arc<Element>]) -> Vec<&'static str> {
    elements.iter().map(|e| match &e.kind {
        ElementKind::Symbol { .. } => "symbol",
        ElementKind::Text(_) => "text",
        ElementKind::Shape(_) => "shape",
        ElementKind::Group { .. } => "group",
        _ => "other",
    }).collect()
}

// Groups used more than once.

/// A group: a `CPicShape` with no artwork of its own whose members `members` writes (given
/// the group's own number).
fn group(w: &mut Ar, members: impl FnOnce(&mut Ar, u16)) -> u16 {
    let n = w.object("CPicShape", 0, None, 0, 0x03, |w| {
        let me = w.count - 1;
        members(w, me);
    });
    w.empty_shape();
    w.i32(0); // no cubic hints
    n
}

/// A drawing object holding the rectangle.
fn drawing(w: &mut Ar) {
    w.object("CPicShape", 0, None, 1, 0x03, |_| {});
    w.out.extend(rectangle_body());
    w.i32(0);
}

/// Groups nested `depth` deep, each holding the one inside it twice (written, then by
/// reference); the innermost holds what `inner` writes.
fn shared(w: &mut Ar, depth: u32, inner: fn(&mut Ar)) -> u16 {
    if depth == 0 {
        return group(w, |w, _| inner(w));
    }
    group(w, |w, _| {
        let n = shared(w, depth - 1, inner);
        w.u16(n);
    })
}

fn shapes_in(elements: &[Arc<Element>]) -> usize {
    elements.iter().map(|e| match &e.kind {
        ElementKind::Shape(_) => 1,
        ElementKind::Group { .. } => shapes_in(&e.kind.group_members()),
        _ => 0,
    }).sum()
}

#[test]
fn a_group_used_twice_comes_in_twice() {
    // Two levels of a group listed twice: four rectangles, nothing left out.
    let (doc, report) = open(page_with(|w| {
        shared(w, 2, drawing);
    }));
    let k = &scene(&doc).layers[0].keyframes[0];
    assert_eq!(kinds(&k.elements), ["group"]);
    assert_eq!(shapes_in(&k.elements), 4);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
}

#[test]
fn a_group_that_contains_itself_comes_in_once_and_says_so() {
    let (doc, report) = open(page_with(|w| {
        group(w, |w, me| {
            drawing(w);
            w.u16(me);
            w.u16(me);
        });
    }));
    let k = &scene(&doc).layers[0].keyframes[0];
    assert_eq!(kinds(&k.elements), ["group"]);
    assert_eq!(shapes_in(&k.elements), 1);
    assert_eq!(skipped(&report, "groups that contain themselves (left out)"), 2, "{:?}", report.skipped);
}

#[test]
fn groups_shared_exponentially_import_in_bounded_work() {
    // 40 levels, each holding the next twice: 41 objects that stand for 2^40 uses. The
    // mapping stops at its budget and says so (it would otherwise never finish).
    let (doc, report) = open(page_with(|w| {
        shared(w, 40, |_| {});
    }));
    assert_eq!(scene(&doc).layers.len(), 1);
    assert!(skipped(&report, "objects past a million in one document (left out)") > 0, "{:?}", report.skipped);
}

// The `Contents` scans.

#[test]
fn a_library_stream_name_without_a_label_does_not_stall_the_scan() {
    // `01 80 17`, a one-unit name, then nothing that reads as a label within the record: the
    // scan moves on, and finds the entry after it.
    let mut c = vec![0x01, 0x80, 0x17, 0x01];
    c.extend(utf16("S"));
    c.extend([0u8; 300]);
    c.extend([0x01, 0x80, 0x17, 0x08]);
    c.extend(utf16("Symbol 9"));
    c.extend([0xFF, 0xFE, 0xFF, 0x04]);
    c.extend(utf16("rays"));
    c.extend(9u32.to_le_bytes());
    c.push(2);
    c.extend([0u8; 8]);
    let items = crate::doc::library_items(&c);
    let got: Vec<(&str, &str, u32, u8)> = items.iter().map(|i| (i.stream.as_str(), i.label.as_str(), i.id, i.kind)).collect();
    assert_eq!(got, [("Symbol 9", "rays", 9, 2)]);
}

/// `Contents` with one `CDocumentPage` record per label (stream `P <n> 1`), each label right
/// behind its record's stream name and the last at the stream's very end.
fn contents_with_scenes(labels: &[&str]) -> Vec<u8> {
    let mut c = vec![0x3F, 0x01, 0x00, 0x00, 0x00];
    for (n, label) in labels.iter().enumerate() {
        c.extend([0xFF, 0xFF, 0x01, 0x00]);
        c.extend(13u16.to_le_bytes());
        c.extend(b"CDocumentPage");
        c.extend([0x17, 0x00]);
        c.extend(utf16(&format!("P {} 1", n + 1)));
        c.extend([0xFF, 0xFE, 0xFF, label.encode_utf16().count() as u8]);
        c.extend(utf16(label));
    }
    c
}

fn scene_names(contents: Vec<u8>) -> Vec<String> {
    let (doc, _) = crate::doc::import(&write(&[("Contents", contents)])).expect("opens");
    doc.scenes.iter().map(|s| s.name.clone()).collect()
}

#[test]
fn a_scene_label_outside_ascii_advances_by_its_stored_units() {
    // 東京 is 2 UTF-16 units (4 bytes in the stream) but 6 bytes of UTF-8: the next record
    // right behind it is still found, and a label at the very end doesn't read past it.
    assert_eq!(scene_names(contents_with_scenes(&["東京", "Scene 2"])), ["東京", "Scene 2"]);
    assert_eq!(scene_names(contents_with_scenes(&["東京"])), ["東京"]);
    assert_eq!(scene_names(contents_with_scenes(&["Scene 1", "Scene 2"])), ["Scene 1", "Scene 2"]);
}

/// The document properties around `legacyLineSpacing`: the stage rectangle (left, right,
/// top, bottom in twips) 74 bytes before the property's name, the background 21 and the
/// frame rate (16.16) 13 bytes before it.
fn properties(rect: [i32; 4], fps: i32) -> Vec<u8> {
    let mut c = vec![0u8; 74];
    for (k, v) in rect.iter().enumerate() {
        c[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
    }
    c[53..57].copy_from_slice(&[0x33, 0x66, 0x99, 0xFF]);
    c[61..65].copy_from_slice(&fps.to_le_bytes());
    c.extend([0xFF, 0xFE, 0xFF, 17]);
    c.extend(utf16("legacyLineSpacing"));
    c
}

#[test]
fn a_stage_rectangle_of_extreme_edges_is_no_stage_and_never_overflows() {
    let s = crate::doc::stage(&properties([0, 11_000, 0, 8_000], 24 << 16)).expect("a stage");
    assert_eq!((s.width, s.height, s.fps), (550, 400, 24.0));
    for rect in [[i32::MIN, i32::MAX, i32::MIN, i32::MAX], [i32::MAX, i32::MIN, i32::MAX, i32::MIN], [0, i32::MAX, 0, 8_000], [i32::MIN, 0, 0, 8_000]] {
        assert!(crate::doc::stage(&properties(rect, 24 << 16)).is_none(), "{rect:?}");
    }
}
