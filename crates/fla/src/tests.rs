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

/// A point as the edge grammar stores it, relative to the pen (`crate::shape`): the three
/// encodings of the flag byte's 2-bit point fields.
#[derive(Clone, Copy)]
enum Pt {
    /// `01`: an i16 pair in 1/256 twip.
    Fine(i16, i16),
    /// `10`: an i32 pair in 1/256 twip.
    Wide(i32, i32),
    /// `11`: an i16 pair in 1/2 twip.
    Half(i16, i16),
}

impl Pt {
    fn code(self) -> u8 {
        match self {
            Pt::Fine(..) => 1,
            Pt::Wide(..) => 2,
            Pt::Half(..) => 3,
        }
    }
    fn write(self, d: &mut Vec<u8>) {
        match self {
            Pt::Fine(x, y) | Pt::Half(x, y) => {
                d.extend_from_slice(&x.to_le_bytes());
                d.extend_from_slice(&y.to_le_bytes());
            }
            Pt::Wide(x, y) => {
                d.extend_from_slice(&x.to_le_bytes());
                d.extend_from_slice(&y.to_le_bytes());
            }
        }
    }
}

/// A style change on an edge record: three u8 indices (`11`) or three u16 (`01`).
#[derive(Clone, Copy)]
enum Styles {
    Narrow(u8, u8, u8),
    Wide(u16, u16, u16),
}

/// Writes one edge record: the flag byte (move, control, anchor, style fields, low to
/// high), then the style indices and the points in that order.
fn edge(d: &mut Vec<u8>, styles: Option<Styles>, mv: Option<Pt>, ctrl: Option<Pt>, anchor: Option<Pt>) {
    let code = |p: Option<Pt>| p.map_or(0, Pt::code);
    let style_code = match styles {
        None => 0,
        Some(Styles::Wide(..)) => 1,
        Some(Styles::Narrow(..)) => 3,
    };
    d.push(code(mv) | code(ctrl) << 2 | code(anchor) << 4 | style_code << 6);
    match styles {
        Some(Styles::Narrow(a, b, c)) => d.extend_from_slice(&[a, b, c]),
        Some(Styles::Wide(a, b, c)) => {
            for v in [a, b, c] {
                d.extend_from_slice(&v.to_le_bytes());
            }
        }
        None => {}
    }
    for p in [mv, ctrl, anchor].into_iter().flatten() {
        p.write(d);
    }
}

/// The start of a named `CPicShape` object as observed: the class's name, the 15-byte
/// object header (`04` … `03`), the matrix (a, b, c, d in 16.16; tx, ty in twips), the
/// `05` byte and the edge count.
fn shape_head(matrix: [i32; 6], edge_count: u32) -> Vec<u8> {
    let mut d = records::ITEM_PROLOGUE.to_vec();
    d.extend_from_slice(&[0xFF, 0xFF, 0x01, 0x00, 0x09, 0x00]); // a new class, schema 1
    d.extend_from_slice(b"CPicShape");
    d.extend_from_slice(&[0x04, 0x00, 0, 0, 0, 0, 0x00, 0x80, 0, 0, 0x00, 0x80, 0x01, 0x00, 0x03]);
    for v in matrix {
        d.extend_from_slice(&v.to_le_bytes());
    }
    d.push(0x05);
    d.extend_from_slice(&edge_count.to_le_bytes());
    d
}

/// The layer-record tail that follows a shape in a library stream.
const TAIL: [u8; 9] = [0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x03];

/// A `CPicShape` record holding the corpus's smallest shape's geometry (Symbol 123, "UFO
/// mask"): a 10.9 × 11.2 px rectangle, white fill at 31 % opacity, its matrix scaled by
/// 65535/65536 and translated to (26.9, −1.55). The bytes are written from the decoded
/// grammar (ADR 0096), not copied from any file.
fn synthetic_shape_record() -> Vec<u8> {
    let mut d = shape_head([0xFFFF, 0, 0, 0xFFFF, 538, -31], 4);
    d.extend_from_slice(&1u16.to_le_bytes()); // one fill:
    d.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0x50, 0x00, 0x00]); // white at 80/255, solid
    d.extend_from_slice(&0u16.to_le_bytes()); // no strokes
    // The right-hand fill 1, a move to (−429, −81) twips, then four straight edges.
    edge(&mut d, Some(Styles::Narrow(0, 0, 1)), Some(Pt::Half(-858, -162)), None, Some(Pt::Half(0, 448)));
    edge(&mut d, None, None, None, Some(Pt::Half(-436, 0)));
    edge(&mut d, None, None, None, Some(Pt::Half(0, -448)));
    edge(&mut d, None, None, None, Some(Pt::Half(436, 0)));
    d.push(0x00); // the edge list's end
    d.extend_from_slice(&TAIL);
    d
}

fn library_shapes(stream: Vec<u8>) -> (Vec<std::sync::Arc<anicraft_doc::Element>>, super::Report) {
    use super::doc;
    let payloads = vec![("Contents", synthetic_contents()), ("P 1 1", synthetic_page()), ("Symbol 9", stream)];
    let (document, report) = doc::import(&write(&payloads)).expect("opens");
    let elements = document.library.iter().find(|s| s.name == "UFO mask")
        .map(|symbol| symbol.timeline.layers[0].keyframes[0].elements.clone())
        .unwrap_or_default();
    (elements, report)
}

fn shape_of(element: &anicraft_doc::Element) -> &anicraft_doc::Shape {
    match &element.kind {
        anicraft_doc::ElementKind::Shape(shape) => shape,
        other => panic!("expected a shape, got {other:?}"),
    }
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

#[test]
fn a_straight_edged_shape_decodes_with_its_geometry_and_fill() {
    let (elements, report) = library_shapes(synthetic_shape_record());
    assert_eq!(elements.len(), 1, "the rectangle decodes as one shape");
    let shape = shape_of(&elements[0]);
    // The fill: white at 80/255.
    let Some(anicraft_doc::Paint::Solid(fill)) = shape.fill else { panic!("a solid fill") };
    assert!((fill.a - 80.0 / 255.0).abs() < 0.001, "fill alpha {fill:?}");
    // The matrix: 65535/65536 scale, translation (26.9, −1.55) px.
    let m = elements[0].transform.m;
    assert!(close(m[0], 65535.0 / 65536.0) && close(m[3], 65535.0 / 65536.0) && m[1] == 0.0 && m[2] == 0.0, "{m:?}");
    assert!(close(m[4], 26.9) && close(m[5], -1.55), "{m:?}");
    // The path: the rectangle's corners, (−429, −81) … (−647, 143) twips, as one closed loop.
    let points: Vec<_> = shape.path.cmds.iter().filter_map(|c| match *c {
        anicraft_geom::PathCmd::MoveTo(p) | anicraft_geom::PathCmd::LineTo(p) => Some((p.x, p.y)),
        anicraft_geom::PathCmd::Close => None,
        _ => panic!("straight edges only"),
    }).collect();
    let want = [(-21.45, -4.05), (-21.45, 7.15), (-32.35, 7.15), (-32.35, -4.05), (-21.45, -4.05)];
    assert_eq!(points.len(), want.len(), "{points:?}");
    for (got, want) in points.iter().zip(want) {
        assert!(close(got.0, want.0) && close(got.1, want.1), "{points:?}");
    }
    assert_eq!(shape.path.cmds.last(), Some(&anicraft_geom::PathCmd::Close));
    assert!(report.skipped.is_empty(), "a fully decoded shape skips nothing: {:?}", report.skipped);
}

/// A shape exercising the whole edge grammar: a radial gradient fill (with a focal point),
/// a solid fill, a square-capped stroke; quadratic edges in all three point encodings; a
/// style change alone; a move; u16 style indices with their flag bit; a region drawn with
/// its fill on the left. Coordinates in twips: a rounded loop from (100, 100) through
/// (140, 140), (100, 180) and (60, 140), then a triangle (300, 100), (400, 200), (300, 200).
fn synthetic_curve_record() -> Vec<u8> {
    let mut d = shape_head([65536, 0, 0, 65536, 0, 0], 7);
    d.extend_from_slice(&2u16.to_le_bytes()); // two fills:
    d.extend_from_slice(&[0, 0, 0, 0, 0x12, 0x00]); // 1: radial gradient
    for v in [65536, 0, 0, 65536, 100, 0] {
        d.extend_from_slice(&i32::to_le_bytes(v)); // its matrix: (5, 0) px
    }
    d.push(2); // two stops
    d.extend_from_slice(&(-51i32).to_le_bytes()); // focal point −51/255 = −0.2
    d.extend_from_slice(&[0; 4]);
    d.extend_from_slice(&[0x00, 0xFF, 0x00, 0x00, 0xFF]); // red at 0
    d.extend_from_slice(&[0xFF, 0x00, 0x00, 0xFF, 0xFF]); // blue at 1
    d.extend_from_slice(&[0x00, 0x80, 0x00, 0xFF, 0x00, 0x37]); // 2: solid green (the second byte varies)
    d.extend_from_slice(&1u16.to_le_bytes()); // one stroke:
    d.extend_from_slice(&[0x00, 0x00, 0x00, 0xFF]); // black
    d.extend_from_slice(&30u16.to_le_bytes()); // 1.5 px
    d.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x02, 0x00, 0x00, 0x05]); // square caps, miter joins, limit 5
    d.extend_from_slice(&[0x00, 0x00, 0x00, 0xFF, 0x00, 0xD9]); // its fill: the colour again
    let fine = |tw: i16| tw * 256; // twips in 1/256 twip
    let wide = |tw: i32| tw * 256;
    let half = |tw: i16| tw * 2;
    // Stroke 1 and fill 1 on the right: a move, then a curve with 01-encoded points.
    edge(&mut d, Some(Styles::Narrow(1, 0, 1)), Some(Pt::Half(half(100), half(100))), Some(Pt::Fine(fine(40), 0)), Some(Pt::Fine(fine(40), fine(40))));
    // A 11-encoded control point and a 10-encoded anchor.
    edge(&mut d, None, None, Some(Pt::Half(0, half(40))), Some(Pt::Wide(wide(-40), wide(40))));
    // The reverse.
    edge(&mut d, None, None, Some(Pt::Wide(wide(-40), 0)), Some(Pt::Half(half(-40), half(-40))));
    // A straight edge back to the start.
    edge(&mut d, None, None, None, Some(Pt::Fine(fine(40), fine(-40))));
    // A style change alone, then u16 indices (fill 2 on the left, flagged) with a move.
    edge(&mut d, Some(Styles::Narrow(0, 0, 0)), None, None, None);
    edge(&mut d, Some(Styles::Wide(0, 0x8002, 0)), Some(Pt::Half(half(200), 0)), None, Some(Pt::Half(half(100), half(100))));
    edge(&mut d, None, None, None, Some(Pt::Half(half(-100), 0)));
    edge(&mut d, None, None, None, Some(Pt::Half(0, half(-100))));
    d.push(0x00);
    d.extend_from_slice(&TAIL);
    d
}

#[test]
fn the_edge_grammar_decodes_curves_styles_and_every_point_encoding() {
    let data = synthetic_curve_record();
    let records = super::shape::find_all(&data);
    assert_eq!(records.len(), 1, "one shape body");
    let r = &records[0];
    let v = |x: f32, y: f32| anicraft_geom::Vec2::new(x / 20.0, y / 20.0); // twips to px
    let seg = |from, ctrl, to| super::shape::Seg { from, ctrl, to };
    let segs: Vec<_> = r.edges.iter().map(|e| (e.stroke, e.fill_left, e.fill_right, e.seg)).collect();
    assert_eq!(segs, vec![
        (1, 0, 1, seg(v(100.0, 100.0), Some(v(140.0, 100.0)), v(140.0, 140.0))),
        (1, 0, 1, seg(v(140.0, 140.0), Some(v(140.0, 180.0)), v(100.0, 180.0))),
        (1, 0, 1, seg(v(100.0, 180.0), Some(v(60.0, 180.0)), v(60.0, 140.0))),
        (1, 0, 1, seg(v(60.0, 140.0), None, v(100.0, 100.0))),
        (0, 2, 0, seg(v(300.0, 100.0), None, v(400.0, 200.0))),
        (0, 2, 0, seg(v(400.0, 200.0), None, v(300.0, 200.0))),
        (0, 2, 0, seg(v(300.0, 200.0), None, v(300.0, 100.0))),
    ]);
    assert!(r.approximations.is_empty(), "{:?}", r.approximations);
}

#[test]
fn a_curved_shape_imports_as_fill_regions_and_strokes() {
    use anicraft_geom::{PathCmd, Vec2};
    let (elements, report) = library_shapes(synthetic_curve_record());
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(elements.len(), 3, "two fill regions, then one stroke");
    let p = |x: f32, y: f32| Vec2::new(x / 20.0, y / 20.0);
    // Fill 1: the rounded loop, its curves as quadratics.
    let first = shape_of(&elements[0]);
    assert_eq!(first.path.cmds, vec![
        PathCmd::MoveTo(p(100.0, 100.0)),
        PathCmd::QuadTo(p(140.0, 100.0), p(140.0, 140.0)),
        PathCmd::QuadTo(p(140.0, 180.0), p(100.0, 180.0)),
        PathCmd::QuadTo(p(60.0, 180.0), p(60.0, 140.0)),
        PathCmd::LineTo(p(100.0, 100.0)),
        PathCmd::Close,
    ]);
    let Some(anicraft_doc::Paint::Radial { stops, focal, .. }) = &first.fill else { panic!("a radial gradient: {:?}", first.fill) };
    assert!(close(*focal, -0.2), "focal {focal}");
    assert_eq!(stops.len(), 2);
    assert!(close(stops[0].offset, 0.0) && close(stops[1].offset, 1.0));
    assert!(close(stops[0].color.r, 1.0) && close(stops[1].color.b, 1.0));
    // Fill 2 lies on the edges' left: its loop runs the triangle backwards.
    let second = shape_of(&elements[1]);
    assert_eq!(second.path.cmds, vec![
        PathCmd::MoveTo(p(400.0, 200.0)),
        PathCmd::LineTo(p(300.0, 100.0)),
        PathCmd::LineTo(p(300.0, 200.0)),
        PathCmd::LineTo(p(400.0, 200.0)),
        PathCmd::Close,
    ]);
    let Some(anicraft_doc::Paint::Solid(green)) = second.fill else { panic!("a solid fill") };
    assert!(close(green.g, 128.0 / 255.0) && close(green.r, 0.0));
    // The stroke: the loop's edges in drawing order, with the decoded style.
    let third = shape_of(&elements[2]);
    let stroke = third.stroke.as_ref().expect("a stroke");
    assert!(close(stroke.width, 1.5));
    assert_eq!((stroke.style.cap, stroke.style.join), (anicraft_doc::Cap::Square, anicraft_doc::Join::Miter));
    assert!(close(stroke.style.miter_limit, 5.0));
    assert!(third.fill.is_none());
    assert_eq!(third.path.cmds.len(), 5, "a move and the four edges: {:?}", third.path.cmds);
    assert!(matches!(third.path.cmds[1], PathCmd::QuadTo(..)));
}

/// `data` with `to` written over it at `at`.
fn patch(mut data: Vec<u8>, at: usize, to: &[u8]) -> Vec<u8> {
    data[at..at + to.len()].copy_from_slice(to);
    data
}

#[test]
fn edge_records_the_grammar_does_not_cover_leave_the_shape_out_and_are_counted() {
    use super::shape::{decode, find_all, ShapeError};
    let good = synthetic_curve_record();
    let body = super::shape::body_after_tag(&good, records::ITEM_PROLOGUE.len() + 6 + 9).expect("the body");
    assert!(decode(&good, body).is_ok());
    // The first edge record's flag: after the fills and the stroke.
    let flag_at = good.iter().enumerate().skip(body).find(|&(i, _)| good[i..].starts_with(&[0x00, 0x00, 0x00, 0xFF, 0x00, 0xD9])).map(|(i, _)| i + 6).expect("the stroke's fill");
    assert_eq!(good[flag_at] >> 6, 3, "the first record changes styles");
    // Style field `10`: a width the corpus never shows.
    let bad = patch(good.clone(), flag_at, &[(good[flag_at] & 0x3F) | 0x80]);
    assert_eq!(decode(&bad, body).err(), Some(ShapeError::StyleWidth));
    // A control point without an anchor.
    let bad = patch(good.clone(), flag_at, &[0x0C]);
    assert!(decode(&bad, body).is_err());
    // An edge count the list doesn't match.
    let bad = patch(good.clone(), body + 1, &8u32.to_le_bytes());
    assert_eq!(decode(&bad, body).err(), Some(ShapeError::Count));
    let bad = patch(good.clone(), body + 1, &6u32.to_le_bytes());
    assert_eq!(decode(&bad, body).err(), Some(ShapeError::Count));
    // A bitmap fill (layout not decoded) leaves the shape out, counted by kind.
    let fill2 = body + 7 + 6 + 24 + 9 + 10;
    assert_eq!(&good[fill2 + 4..fill2 + 6], &[0x00, 0x37]);
    let bad = patch(good.clone(), fill2 + 4, &[0x41]);
    assert_eq!(decode(&bad, body).err(), Some(ShapeError::FillKind(0x41)));
    assert!(find_all(&bad).is_empty());
    let (elements, report) = library_shapes(bad);
    assert!(elements.is_empty());
    assert_eq!(report.skipped.get("shapes with bitmap fills (decode pending)"), Some(&1), "{:?}", report.skipped);
    let (elements, report) = library_shapes(patch(good.clone(), flag_at, &[0x0C]));
    assert!(elements.is_empty());
    assert_eq!(report.skipped.get("shapes whose edge records didn't decode"), Some(&1), "{:?}", report.skipped);
}

#[test]
fn a_shape_record_cut_short_anywhere_never_panics_and_never_half_draws() {
    let good = synthetic_curve_record();
    let full_end = super::shape::find_all(&good)[0].end;
    for cut in 0..good.len() {
        let (elements, report) = library_shapes(good[..cut].to_vec());
        if cut < full_end {
            assert!(elements.is_empty(), "cut at {cut}: a partial record drew {} shapes", elements.len());
            if cut > records::ITEM_PROLOGUE.len() + 6 + 9 {
                assert!(!report.skipped.is_empty(), "cut at {cut}: the left-out shape is counted");
            }
        } else {
            assert_eq!(elements.len(), 3, "cut at {cut}");
        }
    }
}
