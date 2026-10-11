# Negative conversion (film scans → positives)

LightCraft can convert a scan of a **film negative** into a positive, in the style of Negative Lab
Pro's *classic* conversion. The maths is a clean-room reimplementation of the behaviour measured in
a separate reverse-engineering project; LightCraft ships **none** of that plugin's data. This page is
the feature's reference and its honest gap list.

## What it does

Given a scan, the conversion

1. measures the **film base** from the frame's border ring as the ring's per-channel **95th
   percentile over its chromatic pixels** (the bright unexposed margin; a high percentile, since the
   plugin's own base behaves like a per-frame per-channel maximum — robust to a hot pixel on the
   margin, and restricted to coloured pixels since a scanner holder's neutral white margin is
   brighter than the orange base and would otherwise be read instead). The ring the base is read
   from is **chosen from the frame** (`negative.border` defaults to auto): the outermost ring
   dominated by chromatic pixels. If the settings arrive with no measured base — a settings file, a
   preset, copy/paste — the pipeline stage measures it here from the frame's own border, so a
   missing base never becomes a garbage positive (the degenerate `base = 0` window is refused,
   treated as the identity),
2. inverts **per channel** with a linear window, `out = clamp01((base − in) / (base − dark))`, mapping
   the film base to black and the dark endpoint to white. `dark` is the analysed frame's per-channel
   **robust near-min** when `negative.dark_auto` is set (the conversion commands set it, so a user's
   conversion does not depend on the placeholder), and an explicit `negative.darkR/G/B` otherwise
   (Gaps 1),
3. neutralises/applies the modelled **preset vocabulary** (density, clips, tone profile, region
   colour, fade/glow, LUT),
4. applies a **camera profile** when the user supplies one, and
5. encodes the result — producing a positive in the pipeline's normal scene-linear space.

The pipeline runs the conversion in its **scene-linear stage** (`lightcraft_pipeline::negative`). With
the conversion **on**, the white balance is applied **first**, on the negative — and for a negative it
is not a colour temperature at all: an explicit white balance (`wb.mode != AsShot`) is the
**film-base neutralization** (below). `wb.mode == AsShot` keeps the scan as it came — the confirmed
invert window already neutralizes the base per channel, and the reference fixtures pin that path. The
film base is measured from the as-shot scan before that white balance moves it, and the invert
window's high end is the **white-balanced** base (the plugin's recorded `blackPointOrigin` is
measured on the analysed frame, after its white balance). The conversion then inverts; tone and the
remaining edits follow. Because the confirmed window acts on the scan's *encoded* values, the stage
round-trips the scene-linear Rec.2020 buffer through encoded sRGB and back, so the rest of the
pipeline is unchanged.

### The white balance of a negative is the base neutralization

A film negative has no scene illuminant to balance for: the plugin's recorded per-photo
`Temperature`/`Tint` (2000–2650 K on real colour scans) is how it serializes a **per-channel gain
neutralization of the sampled film base** into a Lightroom control — its own per-roll state stores
exactly such gains (`greenMultiplier`/`blueMultiplier`, "red is the reference channel"). Three
measurements pin this down and rule the colour-temperature model out:

- the recorded post-WB base is **neutral** for the current engine's colour frames (channel spread
  ≤ 5 code values at levels 86–146), and neutral by construction for the black-and-white ones;
- no Kelvin value reproduces that through a colour-temperature adaptation: LightCraft's adaptation
  of the developed pixels from the as-shot white to 2000–2650 K leaves the base's green pinned
  (~215) while the red channel goes *negative* in linear below 2856 K (−30 at 2856 K, −1055 at
  2000 K) and the blue pegs at 255 from ~4200 K down — the matrix action itself is smooth and
  monotone in T, so the earlier "non-monotone ring base" was the border statistic switching between
  the light table and the film as clamping changes which pixels count;
- the whole operation does not depend on a camera colour model (these Sony raws carry none in
  LightCraft: `relative_wb`), and the plugin's serialization is solved against whatever model the
  *rendering* application has — not against a temperature.

So for `negative.enabled` renders with an explicit white balance, `local::wb_matrix_for` returns the
diagonal gain matrix `g = y/base` that neutralizes the measured base at the base's **own Rec.2020
luminance** (`y`) — a white balance must not change exposure, the invariant the rest of the white
balance path upholds — and the Kelvin value plays no part. The recorded faithfulness check
(`blackPointOrigin`, the plugin's base measurement on the analysed frame) then reproduces within
±8 code values on the colour scans, against +145…+162 (blue pegged at the ceiling) before.

`wb.mode == AsShot` is the scan as it came: no white balance, the invert window against the as-shot
base — the path the synthetic fixtures confirm to the 8-bit floor.

## The maths, and where it lives

The conversion lives in the **`lightcraft-negative` crate** (L1):

| file | what |
|---|---|
| `convert.rs` | the confirmed invert window; `linearize`, `neutralise`, `apply_profile`, `apply_preset`, `encode` |
| `color.rs` | matrices, `CalibrationIlluminant`, the camera → XYZ(D50) → working-space chain (public DNG SDK behaviour) |
| `dcp.rs` | reader for Adobe DNG Camera Profiles (`.dcp`, an `IIRC` TIFF) |
| `looktable.rs` | `ProfileHueSatMap` / `ProfileLookTable` trilinear interpolation |
| `tonecurve.rs` | `ProfileToneCurve` piecewise-linear evaluator |
| `params.rs` | the conversion parameters |
| `profiles.rs` | a process-wide cache of user-supplied `.dcp` files |

The **confirmed** stage is `convert::invert`: the per-channel linear window
`out = clamp01((white − in) / (white − dark))`, `white` = the measured film base. On the reference
run's neutral grey wedge this reproduces the shipped plugin's output to the 8-bit floor (see
[Evidence](#evidence)). The remaining stages follow the public DNG Camera Profile chain and the
preset vocabulary; each carries a `HYPOTHESIS` note where the exact maths is behavioural.

## Commands and controls

Every action is a command, dispatched by id from the UI, CLI, control channel and MCP
(`cargo xtask parity` checks the ids):

| command | menu | what |
|---|---|---|
| `develop.negative` | Photo | toggle the conversion; when enabling without a base, measure it from the border, and measure the dark endpoint from the analysed frame (unless an explicit `dark` is given) |
| `develop.negativeMeasure` | Photo | measure the film base from the frame's border ring |
| `develop.negativeDark` | — | set the dark endpoint parameter, or (no `dark`) measure it from the frame |
| `develop.negativeProfile` | — | set/clear the user-supplied `.dcp` path (validated) |
| `develop.negativeProfiles` | — | list the user's own `.dcp` tree (nothing bundled) |
| `develop.negativeProfileAuto` | — | pick the best profile for the active photo's camera and set it |
| `develop.negativeReset` | — | back to the defaults (off) |

The parameters are ordinary develop controls in `Section::Negative`, so `develop.set` /
`develop.adjust` (and thus `lightcraft-cli --set`, the control channel and MCP) address them:

| control id | meaning | default |
|---|---|---|
| `negative.baseR`, `negative.baseG`, `negative.baseB` | film base, per channel (encoded 0..1); `0` = unmeasured → the pipeline measures it | 0 |
| `negative.darkR`, `negative.darkG`, `negative.darkB` | dark endpoint, per channel (encoded 0..1) — the explicit override; used when `dark_auto` is off | grey-wedge fit |
| `negative.dark_auto` | measure the dark endpoint from the analysed frame's per-channel robust near-min (Gaps 1); the conversion commands set it | false |
| `negative.border` | border inset fraction the base is measured over; `-1` = the frame picks it | -1 |
| `negative.profileAmount` | camera-profile blend, 0..200 % | 100 |
| `negative.scanGamma`, `negative.outputGamma` | scan / output gamma | 1.0 (already encoded) |
| `negative.saturation` | preset saturation | 0 |
| `negative.density` | 0 addDensity, 1 neutralDensity | 0 |
| `negative.blackClip`, `negative.whiteClip` | preset clip (negative %) | 0 |
| `negative.toneProfile` | 0 standard, 1 linear, 2 linear_gamma, 3 logarithmic | 1 |
| `negative.lights`, `negative.darks`, `negative.whites`, `negative.blacks` | tone knots | 0 |
| `negative.labFade`, `negative.labGlow` | fade / glow | 0 |
| `negative.shadowsRange`, `negative.highsRange` | region widths | 5 |
| `negative.redCyan*`, `negative.greenMagenta*`, `negative.blueYellow*` | region colour (shadows/mids/highlights) | 0 |
| `negative.processOrder` | 0 colourFirst, 1 toneFirst | 0 |
| `negative.lut` | 0 Crystal, 1 Frontier, 2 Natural, 3 Pakon | 2 (Natural) |

Example (CLI, a settings file — the settings JSON uses the group's snake_case keys). These are the
exact settings behind the evidence table below; `color_density` matters (see the table's notes):

```json
{"negative": {
  "enabled": true,
  "base_r": 0.839, "base_g": 0.663, "base_b": 0.471,
  "tone_profile": 1, "saturation": 0, "color_density": 1, "lut": 2,
  "process_order": 0, "scan_gamma": 1, "output_gamma": 1 }}
```

```
lightcraft-cli render grey_wedge_neg8.png -o positive.png --settings negative.json
```

The `base_*` fields are optional: leave them unset (or `0`) and the conversion **measures the film
base itself** (the ring's per-channel chromatic 95th percentile, below). `develop.negativeMeasure`
records the same measurement into the settings; an explicit `base_*` overrides it. `dark_*` is left
at its placeholder default (the grey-wedge fit).

The measurement is the border ring's per-channel **95th percentile over its chromatic pixels** — a
*high-end* statistic, not a mean, and restricted to pixels whose channel spread clears 0.08. The
film base is the bright unexposed margin, and the reference project shows the plugin stores it as a
per-frame per-channel **maximum**-like value (its `blackPointOrigin` maps to output 0 in 259 of 332
two-point curves exactly, and `roll.rBlack` is the max over the roll's frames: `nlp-re` notes 07
and 08). A bare maximum would be moved by a single hot pixel or speck of dust on the margin, so a
high percentile is used instead. On the `grey_wedge` ring the statistics read: **mean 110/87/62**
(wrong by more than half), **p90 210/166/118** (misses by 4), **p95 214/169/120** (exact), **bare
max 214/169/120** (also exact, but a single hot pixel would move it). p95 is exact on all four
fixtures (214/169/120, 200/155/110, 229/198/151, 214/169/120) while leaving 5 % of the ring free of
the statistic.

The **chromatic** restriction is what makes the real scans work. On the user's ILCE-7M4 scans the
outermost ring carries a bright, near-neutral holder margin; the plain p95 reads it (0.89/0.88/0.89)
and the conversion is over-exposed (the orange film base only inverts correctly when it is the
window's white). Restricting the statistic to chromatic pixels reads the orange base instead
(0.59/0.31/0.22 for `DSC04256`, against the true film base), and the render's per-channel bias drops
from roughly +125 to mixed sign (measured; see the report). No choice of *ring* fixes this on its
own — the neutral margin's bright pixels stay in the ring's top percentile however deep the ring is
taken — which is why the statistic, not only the inset, is corrected. The ring *inset* is still
chosen from the frame: `detect_base_border` returns the outermost ring whose pixels are mostly
chromatic, and `develop.negativeMeasure` reports it (`border` in its result). When the frame shows no
such ring (a dark or neutral holder, or the synthetic fixtures, whose base is a strip on the left
edge) the outermost ring is kept and the result says the ring could not be derived (`border: null`),
rather than falling back to a fixed number.

## Camera profiles: user-supplied, never bundled

The camera profile is an Adobe DNG Camera Profile (`.dcp`). **LightCraft ships none** — the same
rule the project applies to AI model weights. `develop.negativeProfile {"path": "…"}` points at a
`.dcp` the user owns; `lightcraft_negative::profiles::get` reads and validates it once per process.
Setting a path that the colour half cannot use — no `ColorMatrix1`, a 3×4 (four-channel/RGBE)
`ColorMatrix1`, or a tone curve that does not build — **errors**, naming the reason, rather than
accepting a profile the render would ignore. When the path is empty, the profile half is skipped by
design; a path that has since gone missing, oversized or malformed at render time is skipped with a
log line (the render has no error to raise) and the conversion still runs. The preset
parameters are modelled directly (region deltas, density, clips, tone profile, fade/glow, LUT); no
preset file is read.

Every profile the reader accepts is checked end to end against the user's own Negative Lab Pro set
(read-only, by path) in `crates/negative/src/profiles.rs`'s `real_negative_lab_profiles_*` test: all
2589 files parse, and the 24 four-channel-camera profiles are the ones rejected loudly.

### Finding the profile: the Develop panel picker

Negative Lab Pro installs its profiles into Adobe Camera Raw's own tree
(`%APPDATA%\Adobe\CameraRaw\CameraProfiles\NegativeLab Camera Profiles\` on Windows,
`~/Library/Application Support/Adobe/CameraRaw/CameraProfiles/` on macOS, `~/.config/Adobe/…` on
Linux) — three generations of each camera's profile, `base`, `v2` and `v2.3`. LightCraft's Develop
panel (Photo → Negative → **Camera Profile**) shows the current profile and opens a list of every
`.dcp` it finds under those roots, grouped by camera, with **Match this camera** picking the newest
generation for the active photo. `develop.negativeProfiles` lists the same tree (filenames only, no
parsing, so thousands are cheap) and `develop.negativeProfileAuto` does the pick; both search the
standard roots by default and take an explicit `dir` otherwise. `develop.negativeProfileAuto` matches
the photo's Exif `Make Model`/`Model` against the profile filename, prefers `v2.3`, then `v2`, then
`base`, and errors (rather than selecting nothing) when no profile matches. A profile set that lives
somewhere else is reached with the explicit `{"path": "…"}` form of `develop.negativeProfile`.

### The profile must not cast a neutral

A `.dcp`'s `ColorMatrix` maps **camera** RGB; the conversion feeds the profiled stage a display-RGB
image whose neutral is `(1,1,1)`. Applied raw, the matrix turns a neutral magenta: through the
user's ILCE-7M4 v2.3 `ColorMatrix` alone, `(0.5,0.5,0.5)` goes to a green-suppressed cast
(`[0.79,0.34,0.68]` scaled). The matrix is therefore **anchored at the input neutral** — its rows
are scaled so `(1,1,1)` maps to `(1,1,1)` — before the HueSatMap, LookTable, baseline exposure and
tone curve run. The result is a chromaticity correction that leaves neutrals alone; the
`a_camera_profile_keeps_a_neutral_neutral` test holds a neutral at each level to a spread ≤ 0.02
through the whole chain.

On the five real scans (each photo's own white balance, the frame's own measured base, the central
80 % against the stored Lightroom preview) the anchored profile **improves** PSNR on all five, while
before the fix it degraded four of five (measured; see the report). The two candidates in the
question were tested separately: moving the profile **before** the invert (the stage order) was
measured and **rejected** — it lowers PSNR against the preview on four of the five while the
white-point fix raises it on all five — so the profile stays where it was, after the invert, and
only the matrix anchoring changed.

## Gaps (carried forward honestly)

These are unsolved in the reference project too; this feature implements an explicit, documented
parameter where a formula is unknown rather than inventing one.

1. **The dark endpoint's statistic is implemented; its exact percentile is not fitted.** `dark`
   is the input that maps to white. The real plugin derives it from the analysed frame
   (its recorded `whitePointOrigin`), and the evidence now fixes the **form**: a per-channel
   **robust near-min**. `whitePointOrigin` is the across-roll *minimum* of the frames (`roll.rWhite =
   min over frames`), the one fully-known real frame (`DSC05279`) has endpoint 10 against a frame
   minimum of 0 and a p1 of 22, and on the neutral `run-01` grey wedge the fit lands at the 8-bit
   floor just below the input minimum. `lightcraft_negative::convert::measure_dark` implements it as
   the per-channel **0.5th percentile** of the analysed frame — the best-unbiased percentile on the
   frames whose pre-conversion render is faithful (the black-and-white negatives: rms 8–9 code
   values against a target spread of 16), and a *low* percentile so a single speck of dust (the
   frame's bare minimum) cannot move it. It is **opt-in**: `develop.negativeDark` with no `dark`
   (or the Auto button) sets `dark_auto`, and the default stays the documented **placeholder** (the
   grey-wedge fit). The measured endpoint was tried on the user's real negatives against the stored
   previews and **lowered** PSNR on average (see the table in Evidence below), so it is not the
   default; an explicit `negative.darkR/G/B` overrides both. What remains open is the *exact* percentile and any base-relative
   term: the colour negatives — the only frames with a spread of dark content — are not yet faithfully
   reconstructed (gap 4), so the statistic was fitted only on the neutral black-and-white subset. On
   artificial images the difference is visible: on `channel_ramp`, the plugin's scene-derived endpoint
   hard-clips the red ramp (39.8 % of the red row at 255, mean |error| vs the known positive 42.8),
   while a min-like endpoint clips far less — the *residual* is now the percentile, not the family.
2. **The output has a nonlinear colour-mixing component** on top of the per-channel curve. The
   per-channel window cannot reproduce the plugin's colour model, so on the reference `color_grid`
   the two disagree by a warm bias (R−G ≈ +15 code values on the neutral-row patches, which the
   plugin renders warm and this conversion renders neutral) and colour-grid RMS 12–14.
   `apply_preset` models the preset vocabulary *linearly* and does not close this.
3. **The preset maths is behavioural.** `addDensity`, the clips, the tone-profile shapes, `LabFade` /
   `LabGlow` and the LUT gains are the reverse-engineering project's hypotheses, not fitted values.
4. **The profile stage order and white point are partly closed.** The profile is applied **after**
   the invert, on the display-encoded positive, with the shot white fixed to D50; the `ColorMatrix`
   is anchored at the input neutral so it no longer casts a neutral (above), but whether the profile
   belongs on the camera-space negative **before** the invert is still open — applying it before the
   invert was measured against the previews and did not help (it lowers PSNR). The chain's internal
   stage order (matrix → HueSatMap → LookTable → baseline exposure → tone curve) is still a
   hypothesis. Four-channel-camera profiles (3×4 `ColorMatrix`) are refused rather than guessed.
5. **NLP's LUTs, roll analysis and snapshots are out of scope** and not implemented. No plugin data
   is read, decompiled or shipped.

## Evidence

### The user's real negatives (30 photos, against the stored Lightroom previews)

Not in the reproducible test suite (the files are the user's). Each photo: LightCraft's full render
with the photo's own `Temperature`/`Tint`, the user's `Negative Lab - Sony ILCE-7M4 v2.3` profile,
the frame-measured base, and the tone/density/LUT settings NLP wrote; PSNR over the central 80 % against
the preview (the post-conversion positive). Mean over 30 photos (10 per colour model):

| dark endpoint | all 30 | noritsu | frontier | blackandwhite |
|---|--:|--:|--:|--:|
| placeholder (default) | **9.54 dB** | 8.12 | 11.82 | 8.66 |
| measured near-min (`dark_auto`) | 8.44 dB | 7.88 | 10.90 | 6.55 |

The measured endpoint is worse on average, so it stays opt-in. The endpoint estimate itself is closer
to the recorded `whitePointOrigin` on black-and-white frames (mean |error| 6.9 code values against 24.4
for the placeholder) but not on colour frames (37 and 15 against 43 and 23), and the render error that
dominates PSNR sits elsewhere: on the black-and-white frames the render's dark end is 100–150 code
values above the preview's, because the colour render (raw white balance and camera profile at
2000–2650 K) is not the frame NLP analysed. Until that stage is faithful the endpoint
cannot be judged on PSNR.

Every row comes from **the same command with the same settings**, and the film base is **measured**
(no `base_*` in the settings) — the strongest claim, and what a user gets. All of it is reproduced by
`apps/lightcraft-cli/tests/negative_reference.rs`, which also asserts the measured base equals each
fixture's uniform strip within one code value. The settings are the JSON example above: a linear tone
profile, `saturation: 0`, **`color_density: 1`** (neutral — it matters, see below), the Natural LUT,
colour-first, gamma 1 in and out; `dark_*` is the placeholder default. The measured bases are
`grey_wedge` and `photo_landscape` 214/169/120, `color_grid` 200/155/110, `channel_ramp` 229/198/151.

```
for f in grey_wedge color_grid channel_ramp photo_landscape; do
  lightcraft-cli render testdata/negatives/${f}_neg8.png -o lc_${f}.png --settings ${f}.json
done   # ${f}.json has no base_*: the base is measured
```

Reproduce (prints this table; skips without the fixtures, like the craft-fonts tests):

```
NLP_RE_DIR=~/git/ai/nlp-re cargo +stable test -p lightcraft-cli \
    --test negative_reference -- --nocapture
```

All figures are 8-bit code values over the scene region (right 94 %; the left 6 % is the film-base
strip).

| image | comparison | R / G / B RMS | max | note |
|---|---|---|---|---|
| `grey_wedge` | this conversion vs the plugin's export | **1.76 / 1.38 / 4.37** | 4 / 3 / 10 | the confirmed window; at the 8-bit floor |
| `grey_wedge` | plugin's export vs the known positive | 15.9 / 15.9 / 16.5 | 26 | the known positive is a different (synthetic) forward model |
| `grey_wedge` | this conversion vs the known positive | 16.9 / 15.6 / 15.3 | 25 | as close as the plugin is; both share the same bias |
| `color_grid` | this conversion vs the plugin's export | 12.4 / 14.4 / 13.8 | 43 | gap 2 (colour mixing) |
| `channel_ramp` | this conversion vs the plugin's export | 35.3 / 23.7 / 19.1 | 98 | gap 1 (dark endpoint) |
| `photo_landscape` | this conversion vs the known positive | 17.5 / 15.4 / 8.6 | 29 | no plugin export for this frame |

The `color_density` note: at the **default** density (`color_density: 0`, addDensity) the
`grey_wedge` row is **6.24 / 5.75 / 7.39** (max 12 / 9 / 12). The plugin's reference export is the
per-channel window alone, so the neutral `color_density: 1` is what reproduces it; that setting is
now stated in the example, so the table is reachable from the doc. (The values above were produced
with `color_density: 1` throughout and did not change when the settings were pinned down.)

The grey-wedge film base **measured** from the border ring is exactly `214/169/120`, matching the
film-base strip, and the conversion maps it to exactly black — so `{"negative": {"enabled": true}}`
alone now reproduces the `grey_wedge` row (RMS 1.76 / 1.38 / 4.37, was 64.5 / 64.0 / 64.0 with the old
mean statistic). Other tests that exercise it: the settings path with no base
(`render_negative_settings_measures_the_base` in `apps/lightcraft-cli/tests/cli.rs`) and the pipeline
unit tests in `crates/pipeline/src/negative.rs`.
