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
fn the_corpus_decodes_straight_edged_library_shapes() {
    let Some(dir) = corpus() else { return };
    let mut total = 0usize;
    let mut files = 0usize;
    for entry in std::fs::read_dir(&dir).expect("corpus directory lists") {
        let path = entry.expect("entry").path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        if !name.ends_with("-legacy.fla") { continue; }
        let data = std::fs::read(&path).expect("file reads");
        if !anicraft_fla::is_legacy(&data) { continue; }
        let (doc, _) = anicraft_fla::import(&data).expect("import");
        let shapes: usize = doc.library.iter()
            .map(|s| s.timeline.layers.iter().map(|l| l.keyframes.iter().map(|k| k.elements.len()).sum::<usize>()).sum::<usize>())
            .sum();
        if shapes > 0 { files += 1; }
        total += shapes;
    }
    assert!(total >= 10, "expected real straight-edged shapes to decode, got {total}");
    assert!(files >= 3, "expected several files with decoded artwork, got {files}");
}
