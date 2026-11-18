# ADR 0096: Legacy Flash import — the compound binary container

Date: 2026-11-16 · Status: accepted — the container (Stage 1) and the structure pass
(scenes, layers, library; Stage 2 part 1) are implemented; record-payload decode continues

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
   `Media`: `03 05 60 01 58`, library items: `01 FF FF 01 00`), then records whose names
   are 16-bit little-endian length-prefixed ASCII: `CPicPage → CPicLayer → CPicFrame`
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
  importer does. Library symbols show each item's first shape record; the other records
  are placed by the frame pass, so the stage stays empty until placements land. Bitmap
  fills and stroke kinds (dashed, dotted, hairline) are not yet located.
- **What stays open**: frame records (durations are u16 LE near keyframes; the exact
  keyframe record layout is not yet mapped), tweens, text, bitmaps
  and sounds — each counted in the report's `skipped` per kind, and a sound file's name
  inside a frame currently reads as an extra layer (the record-level pass will tell them
  apart). Committed fixtures stay synthetic; the CC BY-NC-SA corpus runs via
  `ANICRAFT_FLA_CORPUS`.

## References

- Microsoft `[MS-CFB]`: Compound File Binary Format specification (public).
- The PR's task list and the local corpus notes (not committed; EDAP samples are CC BY-NC-SA).
