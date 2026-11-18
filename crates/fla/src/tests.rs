//! Container tests, with a compound-file writer built independently from the reader —
//! from Microsoft [MS-CFB] alone, like the hardened zip reader's corpus writer (ADR 0054).

use super::cfb::{Compound, Error, Kind, MAX_FILE};
use super::records;

/// Builds a version-3 compound file holding named streams. Streams shorter than the
/// 4096-byte cutoff go through the mini stream, longer ones through the FAT — so both
/// paths are exercised by construction. (`name`, `bytes`) pairs; names are ASCII.
fn write(streams: &[(&str, Vec<u8>)]) -> Vec<u8> {
    const SECTOR: usize = 512;
    const CUTOFF: usize = 0x1000;
    const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
    const FREESECT: u32 = 0xFFFF_FFFF;

    // Layout: header, then big streams, then the mini stream, the directory, the mini
    // FAT, and the FAT. The header comes back and is filled in last.
    let mut out = vec![0u8; 512];
    let mut pos = 512usize;

    let (mini, big): (Vec<_>, Vec<_>) = streams.iter().map(|(n, b)| (n.to_string(), b.clone())).partition(|(_, b)| b.len() < CUTOFF);

    // Big streams: whole sectors each.
    let mut big_starts: Vec<(String, u32, u64)> = Vec::new();
    for (name, bytes) in &big {
        let first = (pos / SECTOR - 1) as u32;
        big_starts.push((name.clone(), first, bytes.len() as u64));
        let mut padded = bytes.clone();
        if let Some(rem) = padded.len().checked_rem(SECTOR) && rem > 0 {
            padded.resize(padded.len() + (SECTOR - rem), 0);
        }
        out.extend_from_slice(&padded);
        pos += padded.len();
    }

    // The mini stream: every small stream's bytes, packed into 64-byte mini sectors,
    // then padded to whole sectors.
    let mut mini_stream = Vec::new();
    let mut mini_starts: Vec<(String, u32, u64)> = Vec::new();
    for (name, bytes) in &mini {
        let first = (mini_stream.len() / 64) as u32;
        mini_starts.push((name.clone(), first, bytes.len() as u64));
        mini_stream.extend_from_slice(bytes);
        if let Some(rem) = bytes.len().checked_rem(64) && rem > 0 {
            mini_stream.resize(mini_stream.len() + (64 - rem), 0);
        }
    }
    let mut mini_stream_sectors: Vec<u32> = Vec::new();
    if !mini_stream.is_empty() {
        let first = (pos / SECTOR - 1) as u32;
        let rem = mini_stream.len() % SECTOR;
        if rem > 0 {
            mini_stream.resize(mini_stream.len() + (SECTOR - rem), 0);
        }
        out.extend_from_slice(&mini_stream);
        for s in 0..mini_stream.len() / SECTOR {
            mini_stream_sectors.push(first + s as u32);
        }
        pos += mini_stream.len();
    }

    // The directory: a root entry plus one per stream; the tree is a flat sibling chain
    // from the root's child, each entry's right sibling the next.
    let mut entries: Vec<Vec<u8>> = Vec::new();
    let name_bytes = |name: &str| -> Vec<u8> { name.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let mut root = vec![0u8; 128];
    let root_name = name_bytes("Root Entry");
    root[..root_name.len()].copy_from_slice(&root_name);
    root[64] = (root_name.len() + 2) as u8;
    root[66] = 5; // the root storage
    for at in [68, 72] {
        root[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    }
    root[120..128].copy_from_slice(&(mini_stream.len() as u64).to_le_bytes());
    entries.push(root);    for (name, _) in streams {
        let mut e = vec![0u8; 128];
        let bytes = name_bytes(name);
        e[..bytes.len()].copy_from_slice(&bytes);
        e[64] = (bytes.len() + 2) as u8;
        e[66] = 2; // a stream
        // Unused tree links are NOSTREAM, not zero: 0 is a valid directory id.
        for at in [68, 72, 76] {
            e[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        }
        entries.push(e);
    }
    for (name, first, size) in big_starts.iter().chain(&mini_starts) {
        let e = entries.iter_mut().find(|e| e.starts_with(&name_bytes(name))).expect("written entry");
        e[116..120].copy_from_slice(&first.to_le_bytes());
        e[120..128].copy_from_slice(&size.to_le_bytes());
    }
    for i in 1..entries.len() {
        if i + 1 < entries.len() {
            entries[i][72..76].copy_from_slice(&(i as u32 + 1).to_le_bytes());
        }
    }
    if entries.len() > 1 {
        entries[0][76..80].copy_from_slice(&1u32.to_le_bytes());
    }
    if let Some(&first) = mini_stream_sectors.first() {
        entries[0][116..120].copy_from_slice(&first.to_le_bytes()); // the mini stream
    } else {
        entries[0][116..120].copy_from_slice(&ENDOFCHAIN.to_le_bytes());
    }
    let dir_start = (pos / SECTOR - 1) as u32;
    let dir_sectors = (entries.len() * 128).div_ceil(SECTOR);
    let mut dir_padded = entries.concat();
    dir_padded.resize(dir_sectors * SECTOR, 0xFF);
    out.extend_from_slice(&dir_padded);
    pos += dir_padded.len();

    // The mini FAT: one entry per 64-byte mini sector, chaining each small stream.
    let mini_sectors_total = mini_stream.len() / 64;
    let mut minifat = vec![ENDOFCHAIN; mini_sectors_total.max(1)];
    for (_, first, size) in &mini_starts {
        let n = (*size as usize).div_ceil(64);
        for i in 0..n {
            let id = *first as usize + i;
            minifat[id] = if i + 1 < n { *first + i as u32 + 1 } else { ENDOFCHAIN };
        }
    }
    let minifat_start = (pos / SECTOR - 1) as u32;
    let mut mf_padded = minifat.iter().flat_map(|v| u32::to_le_bytes(*v)).collect::<Vec<u8>>();
    let mf_sectors = mf_padded.len().div_ceil(SECTOR);
    mf_padded.resize(mf_sectors * SECTOR, 0xFF);
    out.extend_from_slice(&mf_padded);
    pos += mf_padded.len();

    // The FAT: the FAT's own chain first, then the big streams, the directory, the mini
    // stream and the mini FAT; everything else free.
    let fat_start = (pos / SECTOR - 1) as u32;
    let fat_entries_needed = pos / SECTOR + 1;
    let fat_sectors = fat_entries_needed.div_ceil(SECTOR / 4);
    let mut fat = vec![FREESECT; fat_sectors * SECTOR / 4];
    for (i, entry) in fat.iter_mut().enumerate().take(fat_sectors) {
        *entry = if i + 1 < fat_sectors { fat_start + i as u32 + 1 } else { ENDOFCHAIN };
    }
    let chain = |fat: &mut Vec<u32>, first: u32, count: usize| {
        for i in 0..count {
            let id = first as usize + i;
            fat[id] = if i + 1 < count { first + i as u32 + 1 } else { ENDOFCHAIN };
        }
    };
    for (_, first, size) in &big_starts {
        chain(&mut fat, *first, (*size as usize).div_ceil(SECTOR));
    }
    chain(&mut fat, dir_start, dir_sectors);
    if !mini_stream_sectors.is_empty() {
        chain(&mut fat, mini_stream_sectors[0], mini_stream_sectors.len());
    }
    chain(&mut fat, minifat_start, mf_sectors);
    out.extend_from_slice(&fat.iter().flat_map(|v| u32::to_le_bytes(*v)).collect::<Vec<u8>>());

    // The header, finally.
    let h = &mut out[..512];
    h[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
    h[24..26].copy_from_slice(&0x003Eu16.to_le_bytes()); // minor version
    h[26..28].copy_from_slice(&3u16.to_le_bytes()); // major version, 512-byte sectors
    h[28..30].copy_from_slice(&0xFFFEu16.to_le_bytes()); // little-endian
    h[30..32].copy_from_slice(&9u16.to_le_bytes());
    h[32..34].copy_from_slice(&6u16.to_le_bytes()); // 64-byte mini sectors
    h[44..48].copy_from_slice(&(fat_sectors as u32).to_le_bytes());
    h[48..52].copy_from_slice(&dir_start.to_le_bytes());
    h[56..60].copy_from_slice(&0x1000u32.to_le_bytes()); // mini-stream cutoff
    h[60..64].copy_from_slice(&minifat_start.to_le_bytes());
    h[64..68].copy_from_slice(&(mf_sectors as u32).to_le_bytes());
    h[68..72].copy_from_slice(&ENDOFCHAIN.to_le_bytes()); // no extended DIFAT
    h[72..76].copy_from_slice(&0u32.to_le_bytes());
    for i in 0..109 {
        let id = if i < fat_sectors { fat_start + i as u32 } else { FREESECT };
        h[76 + i * 4..80 + i * 4].copy_from_slice(&id.to_le_bytes());
    }
    out
}

fn entry_record(f: &[u8], name: &str) -> usize {
    let needle = name.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
    let at = f.windows(needle.len()).position(|w| w == needle).expect("entry found");
    at - (at % 128) // the entry's 128-byte record, absolutely addressed
}

fn set_u32(f: &mut [u8], at: usize, v: u32) {
    f[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

#[test]
fn streams_round_trip_through_the_container() {
    let payloads: Vec<(&str, Vec<u8>)> = vec![
        ("Contents", b"hello document".to_vec()),
        ("S 1 100", (0..5000).map(|i| (i % 251) as u8).collect()),
        ("Media 1", vec![0xAA; 300]),
        ("Symbol 1", b"x".to_vec()),
        ("S 2 100", vec![0x55; 4096]), // exactly the cutoff: the FAT path
    ];
    let bytes = write(&payloads);
    let file = Compound::new(&bytes).expect("our own writer's output opens");
    let names: Vec<&str> = file.entries().iter().filter_map(|e| if e.path.is_empty() { None } else { Some(e.path.as_str()) }).collect();
    assert_eq!(names, vec!["Contents", "S 1 100", "Media 1", "Symbol 1", "S 2 100"]);
    for (name, want) in &payloads {
        let got = file.stream(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(got, *want, "{name}");
    }
    assert_eq!(file.entries()[0].kind, Kind::Storage);
}

#[test]
fn a_file_that_is_not_a_compound_file_is_refused() {
    assert!(matches!(Compound::new(b"PK\x03\x04 whatever"), Err(Error::NotCompound)));
    assert!(matches!(Compound::new(&[]), Err(Error::NotCompound)));
}

#[test]
fn a_truncated_file_is_refused() {
    let good = write(&[("Contents", b"hi".to_vec())]);
    for cut in [100usize, 512, 1024, good.len() - 1] {
        assert!(matches!(Compound::new(&good[..cut]), Err(Error::Truncated | Error::NotCompound)), "cut at {cut}");
    }
}

#[test]
fn a_fat_chain_that_loops_is_refused() {
    let mut f = write(&[("Contents", vec![0; 8192])]); // a multi-sector stream
    // Find the FAT sector by its chain words: the big stream's chain (sector 1 → 2, 3, …)
    // sits after the FAT's own chain words, so look for `2, 3` as the second and third words.
    let fat_at = (0..f.len()).step_by(512)
        .find(|&c| f[c + 4] == 2 && f[c + 5] == 0 && f[c + 6] == 0 && f[c + 7] == 0 && f[c + 8] == 3 && f[c + 12] == 4)
        .expect("the FAT sector found by its chain words");
    // Every chain word that names a real sector becomes a self-loop.
    for w in 0..(f.len() - fat_at) / 4 {
        let at = fat_at + w * 4;
        let v = u32::from_le_bytes(f[at..at + 4].try_into().unwrap());
        if v != ENDOFCHAIN && v != FREESECT && v != FATSECT_MARKER && v > 0 && v < (f.len() / 512) as u32 {
            set_u32(&mut f, at, w as u32);
        }
    }
    // Opening validates the header, directory and mini-FAT chains; the stream's own
    // chain is walked when the stream is read.
    if let Ok(file) = Compound::new(&f) {
        assert!(matches!(file.stream("Contents"), Err(Error::ChainCycle | Error::Truncated)), "the looping stream chain must be refused");
    }
}

const ENDOFCHAIN: u32 = 0xFFFF_FFFE;
const FREESECT: u32 = 0xFFFF_FFFF;
const FATSECT_MARKER: u32 = 0xFFFF_FFFC;

#[test]
fn a_header_that_lies_about_its_structure_is_refused() {
    let mut f = write(&[("Contents", b"hi".to_vec())]);
    f[30] = 10; // a sector shift that doesn't match version 3
    assert!(matches!(Compound::new(&f), Err(Error::BadHeader(_) | Error::Truncated)));

    let mut f = write(&[("Contents", b"hi".to_vec())]);
    f[44] = 99; // more FAT sectors than the header lists
    assert!(matches!(Compound::new(&f), Err(Error::BadHeader(_) | Error::Truncated | Error::ChainCycle)));

    let mut f = write(&[("Contents", b"hi".to_vec())]);
    f[28] = 0; // the byte order mark broken
    assert!(matches!(Compound::new(&f), Err(Error::BadHeader(_))));
}

#[test]
fn a_directory_tree_that_loops_is_refused() {
    let mut f = write(&[("Contents", b"hi".to_vec()), ("S 1", b"there".to_vec())]);
    let record = entry_record(&f, "Contents");
    // Entry 1's right sibling points back at entry 1 (ids count the root as 0).
    set_u32(&mut f, record + 72, 1);
    assert!(matches!(Compound::new(&f), Err(Error::TreeCycle | Error::BadHeader(_) | Error::Truncated)));
}

#[test]
fn a_stream_larger_than_the_cap_is_refused() {
    let mut f = write(&[("Contents", b"hi".to_vec())]);
    let record = entry_record(&f, "Contents");
    let big = (256u64 << 20) + 1;
    f[record + 120..record + 128].copy_from_slice(&big.to_le_bytes());
    assert!(matches!(Compound::new(&f), Err(Error::StreamTooLarge | Error::Truncated | Error::ChainCycle)));
}

#[test]
fn the_size_cap_matches_the_adr_0054_profile() {
    assert_eq!(MAX_FILE, 1 << 30);
}

#[test]
fn stream_names_with_spaces_and_digits_round_trip() {
    let payloads = vec![("S 1 1724827326", b"ok".to_vec())];
    let bytes = write(&payloads);
    let f = Compound::new(&bytes).expect("opens");
    assert_eq!(f.entries()[1].path, "S 1 1724827326");
    assert_eq!(f.stream("S 1 1724827326").expect("reads"), b"ok".to_vec());
}

/// A minimal legacy document's `Contents`: the version byte, then the `CDocumentPage`
/// record — the page's stream name, then the scene's label — all in the observed record
/// encoding (`ff fe ff`-marked UTF-16 strings).
fn synthetic_contents() -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect() };
    let mut c = vec![0x3F, 0x01, 0x00, 0x00, 0x00];
    c.extend_from_slice(&[0xFF, 0xFF, 0x01, 0x00]); // the record stream's marker
    c.extend_from_slice(&13u16.to_le_bytes()); // the record's name length
    c.extend_from_slice(b"CDocumentPage");
    c.push(0x17); // the page record's marker
    c.push(0x00); // its attribute byte
    c.extend_from_slice(&utf16("P 1 1")); // the page's stream name (marker-terminated)
    c.extend_from_slice(&[0xFF, 0xFE, 0xFF, 0x07]); // the scene label: marker + length
    c.extend_from_slice(&utf16("Scene 1"));
    // A library item: the stream-name string (`01 80 17` marker) then its label.
    c.extend_from_slice(&[0x01, 0x80, 0x17, 0x08]);
    c.extend_from_slice(&utf16("Symbol 9"));
    c.extend_from_slice(&[0xFF, 0xFE, 0xFF, 0x08]);
    c.extend_from_slice(&utf16("UFO mask"));
    c.extend_from_slice(&[0u8; 8]);
    c
}

/// A minimal page stream: the item prologue, then two layer records — each the constant
/// layer anchor, the marker, then the name (stored bottom-first).
fn synthetic_page() -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect() };
    let anchor = [0x00u8, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x0B];
    let mut d = records::ITEM_PROLOGUE.to_vec();
    d.extend_from_slice(&[0x08, 0x00]);
    d.extend_from_slice(b"CPicPage");
    for name in ["b", "a"] {
        d.extend_from_slice(&anchor);
        d.extend_from_slice(&[0xFF, 0xFE, 0xFF, name.len() as u8]);
        d.extend_from_slice(&utf16(name));
        d.extend_from_slice(&[0u8; 16]);
    }
    d
}

#[test]
fn a_synthetic_legacy_document_opens_with_its_scenes_and_layers() {
    use super::doc;
    let payloads = vec![
        ("Contents", synthetic_contents()),
        ("P 1 1", synthetic_page()),
        ("S 9 100", b"no records".to_vec()),
    ];
    let bytes = write(&payloads);
    let (document, report) = doc::import(&bytes).expect("the synthetic legacy document opens");
    assert_eq!(document.scenes.len(), 1);
    assert_eq!(document.scenes[0].name, "Scene 1");
    // Layers come bottom-first in the stream; the timeline is top-first.
    let names: Vec<&str> = document.scenes[0].timeline.layers.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b"]);
    // The structure-only open reports what stayed for the record-level decode.
    assert!(report.skipped.is_empty(), "a stream with no known records skips nothing: {:?}", report.skipped);
}

#[test]
fn an_unsupported_version_is_refused_with_the_specific_sentence() {
    use super::doc;
    let mut contents = synthetic_contents();
    contents[0] = 0x2F; // not a Flash 8–CS4 era version byte
    let payloads = vec![("Contents", contents), ("P 1 1", synthetic_page())];
    let bytes = write(&payloads);
    let error = doc::import(&bytes).expect_err("an unsupported version is refused");
    let message = error.to_string();
    assert!(message.contains("unsupported Flash version"), "{message}");
}

/// A `CPicShape` record with the decoded field layout, holding the corpus's smallest
/// shape (Symbol 123, "UFO mask"): a 218×224 px rectangle, white fill at 31 % opacity,
/// its matrix translated to (26.9, −1.55). The bytes are the decoded record's, written
/// from the differential analysis (ADR 0096), not copied from any file.
fn synthetic_shape_record() -> Vec<u8> {
    let mut d = Vec::new();
    d.extend_from_slice(&[0x09, 0x00]);
    d.extend_from_slice(b"CPicShape");
    d.extend_from_slice(&[0x04, 0x00, 0, 0, 0, 0]);
    d.extend_from_slice(&[0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x01, 0x00, 0x03, 0xFF, 0xFF]);
    d.extend_from_slice(&[0u8; 6]);
    d.extend_from_slice(&[0xFF, 0xFF, 0x00, 0x00]);
    d.extend_from_slice(&538i32.to_le_bytes()); // the matrix's x: 26.9 px in twips
    d.extend_from_slice(&(-31i32).to_le_bytes()); // the matrix's y: −1.55 px in twips
    d.push(0x05); // the fill signature
    d.extend_from_slice(&4u32.to_le_bytes()); // the fill style number
    d.extend_from_slice(&[0x01, 0x00]);
    d.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0x50]); // white at 80/255
    d.extend_from_slice(&[0u8; 4]);
    d.extend_from_slice(&[0xF3, 0x00, 0x00]);
    d.push(0x01); // the edge list's start marker
    d.extend_from_slice(&(-858i16).to_le_bytes()); // (−429, −81) px in half-pixels
    d.extend_from_slice(&(-162i16).to_le_bytes());
    d.extend_from_slice(&0i16.to_le_bytes()); // (0, +224) px
    d.extend_from_slice(&448i16.to_le_bytes());
    d.push(0x30); // a straight edge
    d.extend_from_slice(&(-436i16).to_le_bytes()); // (−218, 0) px
    d.extend_from_slice(&0i16.to_le_bytes());
    d.push(0x30);
    d.extend_from_slice(&0i16.to_le_bytes()); // (0, −224) px
    d.extend_from_slice(&(-448i16).to_le_bytes());
    d.push(0x30);
    d.extend_from_slice(&436i16.to_le_bytes()); // (+218, 0) px, closing the loop
    d.extend_from_slice(&0i16.to_le_bytes());
    d.extend_from_slice(&[0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x03]); // the tail
    d
}

#[test]
fn a_straight_edged_shape_decodes_with_its_geometry_and_fill() {
    use super::doc;
    let payloads = vec![
        ("Contents", synthetic_contents()),
        ("P 1 1", synthetic_page()),
        ("Symbol 9", synthetic_shape_record()),
    ];
    let bytes = write(&payloads);
    let (document, report) = doc::import(&bytes).expect("opens");
    let symbol = document.library.iter().find(|s| s.name == "UFO mask")
        .expect("a library symbol is registered");
    let layer = &symbol.timeline.layers[0];
    let elements = &layer.keyframes[0].elements;
    assert_eq!(elements.len(), 1, "the rectangle decodes as one shape");
    let shape = match &elements[0].kind {
        anicraft_doc::ElementKind::Shape(shape) => shape,
        other => panic!("expected a shape, got {other:?}"),
    };
    // The fill: white at 80/255.
    let Some(anicraft_doc::Paint::Solid(fill)) = shape.fill else { panic!("a solid fill") };
    assert!((fill.a - 80.0 / 255.0).abs() < 0.001, "fill alpha {fill:?}");
    // The matrix translation: (26.9, −1.55) px.
    assert!((elements[0].transform.m[4] - 26.9).abs() < 0.001);
    assert!((elements[0].transform.m[5] + 1.55).abs() < 0.001);
    // The path: the rectangle's four corners in half-pixel precision.
    let points: Vec<_> = shape.path.cmds.iter().map(|c| match *c {
        anicraft_geom::PathCmd::MoveTo(p) | anicraft_geom::PathCmd::LineTo(p) => (p.x, p.y),
        _ => panic!("straight edges only"),
    }).collect();
    assert_eq!(points, vec![(-429.0, -81.0), (-429.0, 143.0), (-647.0, 143.0), (-647.0, -81.0), (-429.0, -81.0)]);
    assert!(report.skipped.is_empty(), "a fully straight shape skips nothing: {:?}", report.skipped);
}



