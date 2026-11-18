//! Corpus tests, run only when the local (uncommitted) ground-truth corpus is available:
//! `ANICRAFT_FLA_CORPUS=/path/to/corpus cargo test -p anicraft-fla --test corpus`.
//!
//! The corpus files are EDAP Tools sample rigs (CC BY-NC-SA, "test/educational only, no
//! commercial"), so they stay out of the MIT-licensed repo; these tests observe behaviour,
//! they don't commit fixtures.

use std::path::PathBuf;

fn corpus() -> Option<PathBuf> {
    std::env::var_os("ANICRAFT_FLA_CORPUS").map(PathBuf::from)
}

#[test]
fn every_legacy_fla_in_the_corpus_opens_and_lists_streams() {
    let Some(dir) = corpus() else { return };
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("corpus directory lists") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "fla") {
            continue;
        }
        let data = std::fs::read(&path).expect("file reads");
        if !anicraft_fla::is_legacy(&data) {
            continue; // the zip-wrapped twins of the corpus are not this crate's input
        }
        let streams = anicraft_fla::dump(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(!streams.is_empty(), "{}: no streams", path.display());
        let names: Vec<&str> = streams.iter().map(|s| s.path.as_str()).collect();
        assert!(names.contains(&"Contents"), "{}: no Contents stream ({names:?})", path.display());
        checked += 1;
    }
    if checked == 0 {
        panic!("the corpus directory held no legacy .fla files");
    }
}

#[test]
fn the_corpus_contents_stream_starts_with_the_document_prologue() {
    let Some(dir) = corpus() else { return };
    for entry in std::fs::read_dir(&dir).expect("corpus directory lists") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "fla") {
            continue;
        }
        let data = std::fs::read(&path).expect("file reads");
        if !anicraft_fla::is_legacy(&data) {
            continue;
        }
        let file = anicraft_fla::cfb::Compound::new(&data).expect("opens");
        let contents = file.stream("Contents").expect("Contents reads");
        assert_eq!(&contents[..5], &anicraft_fla::records::CONTENTS_PROLOGUE, "{}", path.display());
    }
}

/// Ground truth from the corpus's modern twins (the same rigs as Animate XFL): scenes
/// named "Scene 1", layers top-first in the twin's DOM order.
fn expected_layers(name: &str) -> Option<Vec<&str>> {
    Some(match name {
        "giskard-legacy.fla" => vec!["notes", "character"],
        "kinefox-legacy.fla" => vec!["notes", "character", "poses"],
        "mademoiselle-legacy.fla" => vec!["notes", "character"],
        "magnebot-legacy.fla" => vec!["notes", "character"],
        "rabbit-legacy.fla" => vec!["notes", "character", "poses"],
        // The audio layer's sound record embeds the sound's file name behind the same
        // anchor as a layer name; the record-level decode pass will tell them apart.
        "movietemplate-legacy.fla" => {
            vec!["notes", "audio", "sound 1", "sound 2", "maracas", "video", "frame counter", "field guide / mask", "fade in / out", "FX", "camera"]
        }
        _ => return None,
    })
}

#[test]
fn the_corpus_opens_with_the_twins_scenes_and_layers() {
    let Some(dir) = corpus() else { return };
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("corpus directory lists") {
        let path = entry.expect("entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(want_layers) = expected_layers(name) else { continue };
        let data = std::fs::read(&path).expect("file reads");
        if !anicraft_fla::is_legacy(&data) {
            continue;
        }
        let (doc, report) = anicraft_fla::import(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(doc.scenes.len(), 1, "{}: one scene", path.display());
        assert_eq!(doc.scenes[0].name, "Scene 1", "{}", path.display());
        let got: Vec<&str> = doc.scenes[0].timeline.layers.iter().map(|l| l.name.as_str()).collect();
        if name == "movietemplate-legacy.fla" {
            // The real layers are all present, top-first, with the sound-name phantom
            // possibly among them.
            for real in &want_layers {
                assert!(got.contains(real), "{}: layer {real:?} missing from {got:?}", path.display());
            }
            assert!(got.len() >= want_layers.len(), "{}", path.display());
        } else {
            assert_eq!(got, want_layers, "{}: layer names top-first like the twin", path.display());
        }
        assert!(!report.skipped.is_empty(), "{}: a structure-only open reports what stayed out", path.display());
        checked += 1;
    }
    assert!(checked >= 6, "expected the corpus's 6 legacy pairs, ran {checked}");
}


#[test]
fn the_corpus_decodes_library_shapes_with_their_curves() {
    let Some(dir) = corpus() else { return };
    let (mut symbols, mut curved, mut files) = (0usize, 0usize, 0usize);
    for entry in std::fs::read_dir(&dir).expect("corpus directory lists") {
        let path = entry.expect("entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.ends_with("-legacy.fla") { continue; }
        let data = std::fs::read(&path).expect("file reads");
        if !anicraft_fla::is_legacy(&data) { continue; }
        let (doc, report) = anicraft_fla::import(&data).expect("import");
        let shapes = |s: &anicraft_doc::Symbol| s.timeline.layers.iter().flat_map(|l| l.keyframes.iter()).flat_map(|k| k.elements.iter()).filter_map(|e| match &e.kind {
            anicraft_doc::ElementKind::Shape(shape) => Some(shape.path.clone()),
            _ => None,
        }).collect::<Vec<_>>();
        let with_art: Vec<_> = doc.library.iter().map(|s| shapes(s)).filter(|p| !p.is_empty()).collect();
        if !with_art.is_empty() { files += 1; }
        symbols += with_art.len();
        curved += with_art.iter().filter(|paths| paths.iter().any(|p| p.cmds.iter().any(|c| matches!(c, anicraft_geom::PathCmd::QuadTo(..))))).count();
        // Every first shape record of the corpus decodes: none is left out.
        let left_out: Vec<_> = report.skipped.keys().filter(|k| k.starts_with("shapes with") || k.starts_with("shapes (edge header") || k.starts_with("shapes whose")).collect();
        assert!(left_out.is_empty(), "{name}: {left_out:?}");
    }
    // 192 symbols (151 with curves) across the six root files when this was written.
    eprintln!("{symbols} library symbols with artwork, {curved} with curves");
    assert!(symbols >= 180, "expected the library's shapes to decode, got {symbols} symbols");
    assert!(curved >= 140, "expected curved artwork, got {curved} symbols with curves");
    assert!(files >= 6, "expected every corpus file to show artwork, got {files}");
}

/// The corpus's legacy files and their Animate twins (a zip package, or the extracted folder
/// where the twin's zip directory is damaged).
const TWINS: &[(&str, &str)] = &[
    ("giskard-legacy.fla", "giskard-animate.fla"),
    ("kinefox-legacy.fla", "kinefox-animate.fla"),
    ("mademoiselle-legacy.fla", "mademoiselle-animate"),
    ("magnebot-legacy.fla", "magnebot-animate.fla"),
    ("movietemplate-legacy.fla", "movietemplate-animate.fla"),
    ("rabbit-legacy.fla", "rabbit-animate.fla"),
    ("robokit-legacy/EDAPT_SMR_FreeStyle_RoboKit_v1b.fla", "robokit-animate/EDAPT_SMR_FreeStyle_RoboKit_v1b_CC.fla"),
    ("storyboard-legacy/EDAPT_Storyboard_Template.fla", "storyboard-animate/EDAPT_Storyboard_Template_CC.fla"),
    ("storyboard-legacy/EDAPT_Storyboard_Template_Sample_Animatic.fla", "storyboard-animate/EDAPT_Storyboard_Template_Sample_Animatic_CC.fla"),
];

/// The twin's library items by name (the file stem, as the legacy `Contents` labels them),
/// each with its XML text.
fn twin_library(path: &std::path::Path) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut add = |name: &str, xml: String| {
        if let Some(stem) = name.strip_suffix(".xml").map(|n| n.rsplit('/').next().unwrap_or(n)) {
            out.entry(stem.to_string()).or_default().push(xml);
        }
    };
    if path.is_dir() {
        let mut dirs = vec![path.join("LIBRARY")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("twin library lists") {
                let p = entry.expect("entry").path();
                if p.is_dir() {
                    dirs.push(p);
                } else if let Ok(xml) = std::fs::read_to_string(&p) {
                    add(&p.to_string_lossy().replace('\\', "/"), xml);
                }
            }
        }
    } else {
        let bytes = std::fs::read(path).expect("twin reads");
        let archive = craft_xfl::Archive::open(&bytes).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let names: Vec<String> = archive.names().filter(|n| n.starts_with("LIBRARY/") && n.ends_with(".xml")).map(str::to_string).collect();
        for name in names {
            if let Ok(Some(xml)) = archive.read(&name) {
                add(&name, String::from_utf8_lossy(&xml).into_owned());
            }
        }
    }
    out
}

/// One twin edge segment in pixels with its styles (stroke, left fill, right fill).
#[derive(Clone, Copy, Debug)]
struct TwinSeg {
    from: (f32, f32),
    ctrl: Option<(f32, f32)>,
    to: (f32, f32),
    styles: (u32, u32, u32),
}

fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let at = tag.find(&format!(" {name}=\""))? + name.len() + 3;
    tag.get(at..).and_then(|rest| rest.find('"').and_then(|end| rest.get(..end)))
}

/// Every `DOMShape` in a twin XML, as its `<Edge>` segments (the `edges` attribute; the
/// `cubics` drawing hints are not edges).
fn twin_shapes(xml: &str) -> Vec<Vec<TwinSeg>> {
    let mut out = Vec::new();
    for shape in xml.split("<DOMShape").skip(1) {
        let shape = shape.split("</DOMShape>").next().unwrap_or("");
        let mut segs = Vec::new();
        for edge in shape.split("<Edge ").skip(1) {
            let tag = format!(" {}", edge.split('>').next().unwrap_or(""));
            let tag = tag.as_str();
            let Some(edges) = attr(tag, "edges") else { continue };
            let edges = edges.replace("&#xD;", " ").replace("&#xA;", " ").replace("&#13;", " ").replace("&#10;", " ");
            let n = |k| attr(tag, k).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            let styles = (n("strokeStyle"), n("fillStyle0"), n("fillStyle1"));
            for s in craft_xfl::edges::parse_edge_segments(&edges) {
                segs.push(TwinSeg { from: (s.from.x, s.from.y), ctrl: s.control.map(|c| (c.x, c.y)), to: (s.to.x, s.to.y), styles });
            }
        }
        out.push(segs);
    }
    out
}

/// Within half a pixel (the requirement; the decodes agree far closer).
fn near(a: (f32, f32), b: anicraft_geom::Vec2) -> bool {
    (a.0 - b.x).abs() <= 0.5 && (a.1 - b.y).abs() <= 0.5
}

/// Whether a decoded record and a twin shape hold the same edges: every segment's end
/// points and control point within 0.5 px, its styles equal (a segment the twin stores
/// reversed has its left and right fills swapped), one to one.
fn same_edges(record: &anicraft_fla::shape::ShapeRecord, twin: &[TwinSeg]) -> bool {
    if record.edges.len() != twin.len() {
        return false;
    }
    let cell = |p: (f32, f32)| (p.0.floor() as i64, p.1.floor() as i64);
    let mut by_to: std::collections::HashMap<(i64, i64), Vec<usize>> = std::collections::HashMap::new();
    for (i, t) in twin.iter().enumerate() {
        by_to.entry(cell(t.to)).or_default().push(i);
    }
    let mut used = vec![false; twin.len()];
    for e in &record.edges {
        let s = e.seg;
        let styles = (e.stroke as u32, e.fill_left as u32, e.fill_right as u32);
        // The nearest unused candidate, as drawn or reversed (tiny segments sit within
        // half a pixel of several others, so the first that fits isn't always the one).
        let d = |a: (f32, f32), b: anicraft_geom::Vec2| (a.0 - b.x).abs() + (a.1 - b.y).abs();
        let ctrl = |t: &TwinSeg| match (t.ctrl, s.ctrl) {
            (Some(a), Some(b)) => near(a, b).then(|| d(a, b)),
            (None, None) => Some(0.0),
            _ => None,
        };
        let forward = |t: &TwinSeg| (near(t.to, s.to) && near(t.from, s.from) && t.styles == styles).then(|| ctrl(t).map(|c| c + d(t.to, s.to) + d(t.from, s.from))).flatten();
        let reverse = |t: &TwinSeg| (near(t.to, s.from) && near(t.from, s.to) && t.styles == (styles.0, styles.2, styles.1)).then(|| ctrl(t).map(|c| c + d(t.to, s.from) + d(t.from, s.to))).flatten();
        let mut found: Option<(f32, usize)> = None;
        for (target, test) in [(s.to, &forward as &dyn Fn(&TwinSeg) -> Option<f32>), (s.from, &reverse)] {
            let (cx, cy) = cell((target.x, target.y));
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for &i in by_to.get(&(cx + dx, cy + dy)).map(Vec::as_slice).unwrap_or(&[]) {
                        if let (false, Some(cost)) = (used[i], test(&twin[i]))
                            && found.is_none_or(|(best, _)| cost < best)
                        {
                            found = Some((cost, i));
                        }
                    }
                }
            }
        }
        let found = found.map(|(_, i)| i);
        match found {
            Some(i) => used[i] = true,
            None => return false,
        }
    }
    true
}

#[test]
fn the_corpus_decodes_curved_shapes_edge_for_edge_like_the_twins() {
    let Some(dir) = corpus() else { return };
    let (mut matched, mut curved, mut ufo) = (0usize, 0usize, false);
    let mut mismatched = Vec::new();
    for (legacy, twin) in TWINS {
        let (legacy_path, twin_path) = (dir.join(legacy), dir.join(twin));
        if !legacy_path.exists() || !twin_path.exists() {
            continue;
        }
        let data = std::fs::read(&legacy_path).expect("legacy reads");
        let file = anicraft_fla::cfb::Compound::new(&data).expect("opens");
        let contents = file.stream("Contents").expect("Contents reads");
        let library = anicraft_fla::doc::library(&contents, &mut anicraft_fla::Report::default());
        let twins = twin_library(&twin_path);
        for (stream, label) in &library {
            let (Ok(bytes), Some(xmls)) = (file.stream(stream), twins.get(label)) else { continue };
            let candidates: Vec<Vec<TwinSeg>> = xmls.iter().flat_map(|x| twin_shapes(x)).collect();
            for record in anicraft_fla::shape::find_all(&bytes).iter().filter(|r| !r.edges.is_empty()) {
                if candidates.iter().any(|t| same_edges(record, t)) {
                    matched += 1;
                    let curves = record.edges.iter().filter(|e| e.seg.ctrl.is_some()).count();
                    if curves > 0 {
                        curved += 1;
                    }
                    // The handoff's fixture: the UFO's lights, 11 loops of 8 quadratics.
                    if *legacy == "movietemplate-legacy.fla" && label == "UFO" && record.edges.len() == 88 && curves == 88 {
                        ufo = true;
                    }
                } else {
                    mismatched.push(format!("{legacy} {stream} ({label}): {} edges", record.edges.len()));
                }
            }
        }
    }
    assert!(mismatched.is_empty(), "{} decoded shapes differ from every twin shape: {:?}", mismatched.len(), &mismatched[..mismatched.len().min(20)]);
    assert!(ufo, "the UFO's 88 quadratic edges match its twin");
    assert!(matched >= 3000, "expected the corpus's ~3200 shapes to match their twins, matched {matched}");
    assert!(curved >= 1500, "expected curved shapes among them, got {curved}");
    eprintln!("{matched} decoded shapes match their twins edge for edge ({curved} with curves)");
}
