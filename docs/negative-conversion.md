# Negative conversion (film scans → positives)

LightCraft can convert a scan of a **film negative** into a positive, in the style of Negative Lab
Pro's *classic* conversion. The maths is a clean-room reimplementation of the behaviour measured in
a separate reverse-engineering project; LightCraft ships **none** of that plugin's data. This page is
the feature's reference and its honest gap list.

## What it does

Given a scan, the conversion

1. measures the **film base** from the frame's border ring as the ring's per-channel **95th
   percentile** (the bright unexposed margin; a high percentile, since the plugin's own base behaves
   like a per-frame per-channel maximum — robust to a hot pixel on the margin). If the settings
   arrive with no measured base — a settings file, a preset, copy/paste — the pipeline stage measures
   it here from the frame's own border, so a missing base never becomes a garbage positive (the
   degenerate `base = 0` window is refused, treated as the identity),
2. inverts **per channel** with a linear window, `out = clamp01((base − in) / (base − dark))`, mapping
   the film base to black and the dark endpoint to white,
3. neutralises/applies the modelled **preset vocabulary** (density, clips, tone profile, region
   colour, fade/glow, LUT),
4. applies a **camera profile** when the user supplies one, and
5. encodes the result — producing a positive in the pipeline's normal scene-linear space.

The pipeline runs it as its **first scene-linear stage** (`lightcraft_pipeline::negative`), before
white balance and tone. Because the confirmed window acts on the scan's *encoded* values, the stage
round-trips the scene-linear Rec.2020 buffer through encoded sRGB and back, so the rest of the
pipeline is unchanged.

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
| `develop.negative` | Photo | toggle the conversion; when enabling without a base, measure it from the border |
| `develop.negativeMeasure` | Photo | measure the film base from the frame's border ring |
| `develop.negativeDark` | — | set the dark endpoint parameter |
| `develop.negativeProfile` | — | set/clear the user-supplied `.dcp` path (validated) |
| `develop.negativeReset` | — | back to the defaults (off) |

The parameters are ordinary develop controls in `Section::Negative`, so `develop.set` /
`develop.adjust` (and thus `lightcraft-cli --set`, the control channel and MCP) address them:

| control id | meaning | default |
|---|---|---|
| `negative.baseR`, `negative.baseG`, `negative.baseB` | film base, per channel (encoded 0..1); `0` = unmeasured → the pipeline measures it | 0 |
| `negative.darkR`, `negative.darkG`, `negative.darkB` | dark endpoint, per channel (encoded 0..1) — **an unfitted placeholder** (the real plugin's rule for it is unknown; see Gaps) | grey-wedge fit |
| `negative.border` | border inset fraction the base is measured over | 0 |
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
base itself** (the ring's per-channel 95th percentile, below). `develop.negativeMeasure` records the
same measurement into the settings; an explicit `base_*` overrides it. `dark_*` is left at its
placeholder default (the grey-wedge fit).

The measurement is the border ring's per-channel **95th percentile** — a *high-end* statistic, not a
mean. The film base is the bright unexposed margin, and the reference project shows the plugin stores
it as a per-frame per-channel **maximum**-like value (its `blackPointOrigin` maps to output 0 in 259 of
332 two-point curves exactly, and `roll.rBlack` is the max over the roll's frames: `nlp-re` notes 07
and 08). A bare maximum would be moved by a single hot pixel or speck of dust on the margin, so a high
percentile is used instead. On the `grey_wedge` ring the statistics read: **mean 110/87/62** (wrong by
more than half), **p90 210/166/118** (misses by 4), **p95 214/169/120** (exact), **bare max
214/169/120** (also exact, but a single hot pixel would move it). p95 is exact on all four fixtures
(214/169/120, 200/155/110, 229/198/151, 214/169/120) while leaving 5 % of the ring free of the
statistic. The ring *geometry* (the reference project's open G2 — how much margin to sample) is still
only partly pinned.

## Camera profiles: user-supplied, never bundled

The camera profile is an Adobe DNG Camera Profile (`.dcp`). **LightCraft ships none** — the same
rule the project applies to AI model weights. `develop.negativeProfile {"path": "…"}` points at a
`.dcp` the user owns; `lightcraft_negative::profiles::get` reads and validates it once per process.
When the path is empty, missing, oversized, unreadable or malformed, the profile half is simply
skipped (logged once) and the conversion still runs — no error the render has to handle. The preset
parameters are modelled directly (region deltas, density, clips, tone profile, fade/glow, LUT); no
preset file is read.

## Gaps (carried forward honestly)

These are unsolved in the reference project too; this feature implements an explicit, documented
parameter where a formula is unknown rather than inventing one.

1. **The dark endpoint is not fitted (the load-bearing unknown).** `dark` is the input that maps to
   white; the real plugin derives it from a *scene statistic* whose rule is unknown and
   content-dependent (its recorded `whitePointOrigin`). It is an explicit parameter
   (`negative.darkR/G/B`, labelled **"Dark Endpoint (unfitted)"** in the Develop panel) and the
   default is the reference grey-wedge fit, a **placeholder**, not discovered behaviour. On
   artificial images the difference is visible: on the reference `channel_ramp`, the plugin's
   scene-derived endpoint hard-clips the red ramp (39.8 % of the red row at 255, mean |error| vs the
   known positive 42.8), while the placeholder endpoint clips 0 % (mean |error| 13.9).
2. **The output has a nonlinear colour-mixing component** on top of the per-channel curve. The
   per-channel window cannot reproduce the plugin's colour model, so on the reference `color_grid`
   the two disagree by a warm bias (R−G ≈ +15 code values on the neutral-row patches, which the
   plugin renders warm and this conversion renders neutral) and colour-grid RMS 12–14.
   `apply_preset` models the preset vocabulary *linearly* and does not close this.
3. **The preset maths is behavioural.** `addDensity`, the clips, the tone-profile shapes, `LabFade` /
   `LabGlow` and the LUT gains are the reverse-engineering project's hypotheses, not fitted values.
4. **The profile stage order** (matrix → HueSatMap → LookTable → baseline exposure → tone curve) is a
   hypothesis; the shot white for the profile is fixed to D50. Four-channel-camera profiles (3×4
   `ColorMatrix`) are refused rather than guessed.
5. **NLP's LUTs, roll analysis and snapshots are out of scope** and not implemented. No plugin data
   is read, decompiled or shipped.

## Evidence

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
