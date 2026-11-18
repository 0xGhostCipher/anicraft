# ADR 0096: Legacy Flash import — the compound binary container

Date: 2026-11-16 · Status: accepted — the container (Stage 1), shapes and the timelines
(scenes, layers, keyframes, instances, tweens, library symbols) are implemented; text, media,
scripts and filters continue

## Context

Files saved by Flash 8 through CS4 — the bulk of the historical Flash authoring corpus —
are OLE2 **compound binary files** (Microsoft `[MS-CFB]`), unlike the zip packages Flash
CS5 and Animate write. The hardened zip reader (ADR 0054) refuses them with the sentence
"this .fla was saved by Flash CS4 or earlier…", which was honest, and
`docs/requirements.md` listed binary FLA as a P1 stretch goal. This ADR covers reading
the container and its record streams; decoding the records into the document model is
follow-on work tracked in `openspec/changes/add-legacy-fla-import/`.

## Decision

1. **Own MS-CFB reader** in a new L2 crate `crates/fla`, from the public specification
   only. Like the workspace's own zip and XML readers: hardening is ours to control
   (1 GiB file cap, 256 MiB stream cap, cycle-guarded chains, in-memory lookups, nothing
   extracted to disk), and `deny.toml`'s dependency policy stays narrow. Alternatives
   considered: an OLE2 crate from crates.io (mixes hardening into a foreign reader; adds
   a dependency for ~600 lines of spec code).

2. **Record streams read as data first.** Observation on real files establishes the
   stream layout: a 5-byte prologue per stream family (`Contents`: `3F 01 00 00 00`,
   `Media`: `03 05 60 01 58`, library items: `01 FF FF 01 00`), then an object graph
   framed the way MFC's `CArchive` writes objects (its documented behaviour, see below),
   whose class names are 16-bit little-endian length-prefixed ASCII: `CPicPage → CPicLayer → CPicFrame`
   (structure), `CPicShape`, `CPicSymbol`, `CPicText`, `CPicSprite`, `CPicButton`,
   `CPicMorphShape` (+ `CMorphSegment`/`CMorphCurve`/`CMorphHintItem`), `CPicBitmap`,
   `CMediaBits`, `CMediaSound`, `CQTAudioSettings`, `CDocumentPage`, `CColorDef`,
   `CObList`. Payloads (geometry, fills, transforms, text) stay to be decoded.

3. **Reverse-engineer differentially.** EDAP Tools ships the same sample rigs in both
   eras ("for Flash 8–CS6" legacy OLE2, "for Animate" modern zip FLA). Decoding is built
   feature-by-feature against the pair: dump the record from the legacy file, find the
   same artwork's node in the modern twin's XML, decode until they agree. The corpus
   (8 pairs, CC BY-NC-SA) lives **outside** the repo for licensing; committed fixtures
   are synthetic, written by a spec-only writer like ADR 0054's.

4. **A microscope command ships now**: `anicraft-cli fla-dump` prints a legacy file's
   stream tree, prologues and record tags as JSON. It is the tool for every later decode
   task and for triaging user files; it also makes the refusal path observable
   (`anicraft-cli info` already names Flash CS4-or-earlier files specifically).

5. **Refusal keeps one sentence.** The reader's errors name one situation each
   (not-a-compound-file, too large, truncated, lying header, looping chain, looping tree,
   over-large stream, malformed name), safe to show, never a panic.

## Consequences

- `crates/fla` joins the workspace at L2 (std + `thiserror` + `anicraft-doc`), with a row in
  the AGENTS.md map and the LAYERS table.
- Record decoding into `anicraft-doc` documents reuses the XFL importer's seam and its
  skipped-report pattern; unknown record kinds are counted, never fatal. The engine's open
  path routes OLE2 bytes to `anicraft_fla::import` before the zip importer sees them.
- **What the structure pass decodes** (differentially verified against the corpus's modern
  twins): the version gate; the `Contents` document index (one `CDocumentPage` per scene:
  page stream name + scene label; the library's stream-name → label pairs); and page
  streams' layer records (constant `00 80 00 00 00 80 00 00 0b` anchor + `ff fe ff`-marked
  UTF-16 name, stored bottom-first). Strings are one-byte-length UTF-16 behind
  `ff fe ff` (names) or `01 80 17` (library stream names) markers.
- **Shape records:** a `CPicShape` body (behind its class name or `0x80nn` class
  reference and a 15-byte object header) holds the full matrix (a–d 16.16, tx/ty i32
  twips), `05`, the edge count, the fill styles (solid RGBA; linear `10` and radial `12`
  gradients with matrix, focal point and stops), the stroke styles (RGBA, width in twips,
  cap/join/miter bytes, a fill), then edge records. Each record's flag byte holds four
  2-bit fields — move, control, anchor, style — whose point codes pick the storage
  (i16 in 1/256 twip, i32 in 1/256 twip, i16 in 1/2 twip), all relative to the pen; style
  changes carry stroke/left-fill/right-fill indices as u8 or u16. A record with a control
  point is a quadratic curve. Verified differentially: every shape body the decoder finds
  in the corpus (3197, 2201 of them curved) matches its twin's XFL edges, styles and
  matrix. (An earlier reading took the edge unit for half-pixels; it is half-twips — the
  "UFO mask" fixture is 10.9 × 11.2 px.) Fill regions are rebuilt per style as the XFL
  importer does. Bitmap fills and stroke kinds (dashed, dotted, hairline) are not yet
  located.
- **Timelines** (`crates/fla/src/archive.rs` holds the full grammar): every item stream is a
  `01` byte and one object graph. Objects are tagged as MFC's `CArchive` tags them — `FF FF`,
  schema, length and name to declare a class; `0x8000 | n` for an object of class `n`; a
  bare `n` for an object written before; `00 00` for none — with classes and objects sharing
  one count. Every `CPic*` object starts with `04`, a flag byte, its children up to `00 00`,
  two i32 (a point in twips, `0x80000000` for none), a u16 and a kind byte. A page's
  children are its layers (bottom-first); a layer's are its keyframes, then its name,
  current/hidden/locked bytes, colour (RGBA), outline, type (0 normal, 1 guide, 3 folder,
  4 mask) and its parent layer as an object reference (a folder is written in place as its
  first child's parent and later back-referenced at its own place in the stack); a
  keyframe's children are its objects, then its raw artwork (a shape body + cubic hints),
  then `18`, the u16 duration, the tween word (low byte none/classic/shape, high byte flags),
  the i16 ease, the sound block, the label, the frame actions, the label kind (1 comment),
  the shape tween's cache and hints (objects), and six custom ease curves (position,
  rotation, scale, colour, filters, all) when flagged. An instance (`CPicSymbol`,
  `CPicSprite` movie clip, `CPicButton`) holds the matrix, first frame, loop mode, colour
  effect (A, R, G, B multipliers in 1/256 and offsets, style, percent, tint), name, the u16
  **library item id from `Contents`** (not the stream number), persistent data, filter count
  and blend (SWF numbering); its base point is the transformation point already placed by
  the matrix. A `CPicShape` object is a drawing object (base u16 1), a loose shape (flag
  bit 0) or a group (members as children). `Contents` gives each library item's u32 id
  and symbol type after its label, and the stage (rectangle in twips, background, 16.16
  frame rate) in the document properties.
- **Skipping what isn't decoded.** Texts, bitmaps, a movie clip's or button's extra
  settings and anything newer are skipped to where their parent continues — its own end
  (`00 00` and its base with its kind byte) or the next sibling, confirmed by parsing that
  sibling — counting class declarations inside the skipped bytes so later class numbers
  stay right. A keyframe whose artwork doesn't decode keeps its timing (its fields are found
  by trial parse). Scan bytes, trial parses, nesting and object counts are capped per
  stream; a timeline that doesn't decode falls back to the structure-only reading.
- **Mapping** mirrors the XFL importer: folders hold no frames, zero-length frame records
  are dropped (the twins don't have them), raw shapes merge (ADR 0042), comments are
  reported, classic and shape tweens keep their eases (shape tweens are rebuilt by the
  engine; the stored morph cache and hints are not used), movie clips and buttons loop.
  A moved transformation point becomes the instance's pivot (ADR 0111).
- **Verified differentially** against all 9 pairs: 337/337 timelines have the twins' layers,
  1628/1628 layers their keyframes (index and duration), 6993/6993 keyframes their tween
  kind, 4457/4457 instances their library item, matrix, loop mode and first frame; the
  stage matches on all pairs; rendered frames of the movie template and the character rigs
  match their twins' apart from text and dotted strokes.
- **What stays open**: text, bitmaps, sounds, frame and button scripts, filters (an
  instance with filters loses its blend mode too), dashed/dotted strokes, instance names
  on buttons, library folders — each counted in the report's `skipped` per kind. Committed
  fixtures stay synthetic; the CC BY-NC-SA corpus runs via `ANICRAFT_FLA_CORPUS`.

## References

- Microsoft `[MS-CFB]`: Compound File Binary Format specification (public).
- Microsoft's MFC documentation of `CArchive` object serialization (`CArchive::WriteObject`,
  `CRuntimeClass` schemas): the public description of the object framing.
- The PR's task list and the local corpus notes (not committed; EDAP samples are CC BY-NC-SA).
