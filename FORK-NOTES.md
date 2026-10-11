# LightCraft fork — what is in here

A fork of [storytold/lightcraft](https://github.com/storytold/lightcraft) carrying work done for one
user's setup: a Sony ILCE-7M4 shooter who came from Lightroom Classic, with a film-scan workflow that
needs a negative conversion LightCraft did not have.

**No pull requests are open from this fork.** It is published so the work is readable, not proposed.

## What this branch carries

**Raw and optics**

- Colour noise reduction on the R−G/B−G basis and noise-measured luminance NR (CPU + GPU), matched
  to Lightroom's luma response; camera and lens metadata kept through import and export.
- Sony embedded lens corrections on the modern convention (ILCE-7M4), applied where they belong: the
  lateral-CA table warps the CFA colour planes *before* demosaic; distortion/vignetting after.
  `EmbeddedLens.lateral_ca` records the file's own CFA table; the "Remove Chromatic Aberration"
  switch (on by default, Lightroom semantics) gates the file's table with the post-demosaic
  estimator as fallback — never both.
- DNG export keeps the lens (`LensSpecification`, `LensMake`, `LensModel`, `LensSerialNumber` in an
  Exif IFD).

**Film negative**

- The conversion is the engine's first scene-linear stage: the film base is measured from the scan's
  chromatic border margin (the ring's per-channel 95th percentile over its chromatic pixels), the
  invert is a per-channel linear window, and the white balance is applied on the *negative* before
  it is inverted — white-balancing an already-inverted positive clips channels.
- The camera profile is a `.dcp` the user owns (LightCraft ships none); an unusable one is rejected
  loudly, and the profile matrix is anchored at the input neutral. The dark endpoint is a
  behavioural scene statistic (the analysed frame's per-channel robust near-min), opt-in; the
  default stays the documented placeholder.
- The film-base white balance neutralises the measured base (per-channel gains, red the reference).
- The redesigned Negative panel: the conversion read in order (switch + measure + ring readout,
  camera profile, measured film base with override flags, border, Auto/Manual dark endpoint with
  read-only measured values, preset vocabulary), `develop.negativeState` answering what the section
  shows.
- Reference fixtures (`cargo +stable test -p lightcraft-cli --test negative_reference`, needs
  `NLP_RE_DIR`) compare the conversion's renders against reference exports measured on real scans.

## Deliberate upstream test divergences

Rebasing the fork's window-geometry work onto upstream's docking suite needed three test-harness
adaptations, kept deliberately (they pin the fixed geometry):

1. **Crop** (`tests_crop_rotate.rs`): the crop-tool harness opens at 1400×800, not upstream's
   1200×800 — the crop panel's controls need the wider window.
2. **Panels** (`tests_panels.rs`): the sidebar-geometry tests use a sized harness
   (`folders_app_sized`, 1400×1100) and the quick-collection count id (`count:quick`), so the
   counts-at-the-visible-edge assertions test the real layout.
3. **Docking** (`tests/docking_workflows.rs`): the harness forces `ViewMode::Detail` — a moved
   panel renders a photo, and the loupe is the view that shows it.

These ride the Library-UI line (`d8ddcc8b`, "Library UI: module bar, docks, filter tabs and grid
captions"); this branch keeps upstream's own UI suite and passes its versions as written.

## What is deliberately *not* here

- **No lens profile database from an outside source.** Lensfun is excluded by this project's rules.
  Nothing is bundled, extracted or depended on; `LIGHTCRAFT_LENSFUN_DIR` stays the opt-in path.
- **No plugin or profile data from a third party.** The negative conversion reads a user-supplied
  camera-profile path and degrades gracefully without it — the same rule the project already applies
  to the AI denoise weights. Nothing is bundled or redistributed.
- **The conversion is reconstructed behaviour.** Its maths comes from the shipped data schemas, from
  public documentation and from black-box observation and fixture fitting; what remains inference is
  recorded as gaps in `docs/negative-conversion.md` rather than presented as fitted.

## Building

The default toolchain is a nightly too old for `epaint 0.36.2`; use the stable toolchain:

```sh
cargo +stable build
cargo +stable test
```

The Windows build the user runs is cross-compiled on Linux (the exact recipe of the current
install; the CRT is linked statically so no redistributable is needed):

```sh
CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS='-C target-feature=+crt-static' \
    cargo +stable build --release --locked -p lightcraft -p lightcraft-cli \
    --features lightcraft/heif,lightcraft-cli/heif --target x86_64-pc-windows-gnu
x86_64-w64-mingw32-strip --strip-all target/x86_64-pc-windows-gnu/release/lightcraft.exe \
    target/x86_64-pc-windows-gnu/release/lightcraft-cli.exe
```
