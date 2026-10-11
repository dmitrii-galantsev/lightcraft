# Sony embedded lens corrections

LightCraft reads the lens-correction tables Sony records in the raw image IFD of every ARW and converts the
distortion table into `OpcodeList3` / `WarpRectilinear`, so the existing profile-correction control, the CPU/GPU
optics path, EXIF orientation handling and DNG export use the same correction (the same path as a DNG's embedded warp
and Panasonic / Leica RW2 distortion). No lens database and no Adobe profile is used: these are the camera's own
numbers.

Two conventions are implemented, each measured against an independent reference and selected by exact camera model:

| Body | Convention | Reference | Enabled |
|---|---|---|---|
| ILCE-7RM4A (A7R IVA) | *older*: unit 16384, the table indexed at the output radius, knots at the crop half diagonal / 15 | Sony Imaging Edge On/Off exports, FE 24–105 and FE 200–600 | yes |
| ILCE-7M4 (A7 IV) | *modern*: unit 15000, the table indexed at the corrected radius (frame-filling zoom), knots at the full-frame crop half diagonal / 15 | the camera's own JPEG, Tamron 35–150 mm F2–2.8 at 92 mm | yes |
| ILCE-7RM6 (A7R VI) | *modern*, as the ILCE-7M4 | the camera's own JPEG, 54 photos, three lenses (PR #824) | yes |

`crates/raw/src/vendor/arw_lens_modern.rs` is the modern convention, driven by a per-body table (full-frame default
crop and the distortion-table knot counts that body was measured with); `crates/raw/src/vendor/arw_lens.rs` is the
older ILCE-7RM4A convention. The two differ in unit and index and no single unit fits every lens under either (the
older one misses the ILCE-7RM6's FE 24–70 GM II by up to 1.43 px), so they stay separate. Matching tag numbers and
table lengths do **not** establish the scale, interpolation or crop normalisation on an unmeasured body, so each body
is listed individually: a new body is a row, added after the same measurement.

This adds **distortion and lateral chromatic aberration**. The lateral-CA table is applied to the raw CFA colour planes
before demosaic (the raw crate's `warp_cfa`, built from the table by `ca_opcode`), not as a post-demosaic
`WarpRectilinear` (applied there it made the render's fringing worse; PR #824). The vignetting table is parsed but not
applied (below: the camera's own JPEG does not correct vignetting either). Linear YCbCr ARWs (a Sony downsized lossless L/M/S) and non-3:2 camera crops are excluded pending
validation; the decoder does not guess an aspect crop's radial normalisation. The camera's in-camera Distortion Off
setting does not erase the table. Newly imported photos use LightCraft's existing embedded-profile default (enabled);
the profile control can disable or scale the distortion.

## Modern convention: the ILCE-7M4 (A7 IV) measurement

The ARW `DSC03836.ARW` (Tamron 35–150 mm F2–2.8, 92 mm, in-camera distortion correction Auto) carries, in the raw
IFD (ExifTool Sony names, count first):

| Table | Tag | Flag | Values |
|---|---|---|---|
| Distortion | `0x7037` | `0x7036` | 16: −4, 0, 11, 27, 49, 75, 110, 149, 198, 253, 318, 392, 476, 571, 681, 807 |
| Vignetting | `0x7032` | `0x7031` | 16: 0, 0, 32, 80, 160, 256, 368, 496, 656, 832, 1024, 1184, 1504, 2112, 3136, 4288 |
| Lateral CA | `0x7035` | `0x7034` | 32: red's 16 knots, then blue's 16 |

The meaning is the modern convention: knot `i` sits at sensor radius `i · s` from the default-crop centre, with
`s = hypot(7008, 4672) / 30` = 280.75 px (the full-frame default crop's half diagonal / 15); a corrected radius `r`
reads the source radius `S(r) = r · (1 + v(r) / 15000)`, and the camera then zooms (`z = 1.0480` here) so the output
fills the crop. The warp's centre is the default crop's centre. The quartic `WarpRectilinear` fit deviates from the
interpolated table by under 0.20 px at a 1440 px long edge, inside the 0.25 px gate.

Method (the project's usual no-chart one). LightCraft's uncorrected render of the ARW (the raw crate's `develop`,
no opcodes) was matched to the camera's own embedded JPEG (the camera applies its correction to it) with
normalised-cross-correlation block matching; the displacement field was fitted for the unit, centre and zoom, the
resulting `WarpRectilinear` was then re-rendered through `OpcodeList3` and re-registered:

| Region (1440 px long edge) | Uncorrected | Corrected |
|---|---:|---:|
| Corner (outer 15 % of the radii) | 11.0 px median | 0.26 px median, 0.50 px max |
| All matched blocks | 14.8 px median | 0.39 px median, 0.62 px 90th |

These are feature residuals, not photometric equality. The camera JPEG and Lightroom Classic's export of the same
file agree to 0.05 px, so either is a valid corrected reference; the numbers above use the camera's own JPEG, as the
project does. The camera setting was Auto (correction on) and the Off JPEG case (which cannot certify an On warp) was
not needed: the corrected render reproduces the camera's.

The ILCE-7RM6 uses the identical convention (unit 15000, the same fitting and frame-filling zoom); its distortion
numbers and tests are in PR #824 and are carried by the shared implementation. The lateral-CA table is applied to the
CFA before demosaic for both bodies (see the lateral-CA section below). A body's knot counts are listed in the same
table, so the ILCE-7M4's 16-knot full-frame layout is the one enabled — a table with another knot count is left
uncorrected until it is measured too.

## Lateral chromatic aberration

Sony's CA table (raw IFD `0x7035`) holds red's knots then blue's; red's (blue's) source radius is
`g · (1 + c(g) / U)` with `g` the green source radius. LightCraft applies it to the **raw CFA colour planes before
demosaic** (`crates/raw/src/vendor/arw_lens_modern.rs`, `crates/raw/src/opcodes.rs::warp_cfa`): the red and blue samples
are resampled onto green's sites while they still sit on their own, then the demosaic interpolates the corrected
colour difference. Applying the same planes after demosaic made the render's fringing worse (PR #824: 0.032 → 0.052 px
rms at 1440), because the demosaic has already attenuated about 70 % of the measured misregistration, so the warp
over-corrects it.

The unit `U` was **refit here** against the ILCE-7M4 camera JPEGs. The render (embedded distortion applied, the
project's `estimate_lateral_ca` as the metric) was measured with the correction off and on at a sweep of units. The
corner R/G and B/G CA falls monotonically as the unit shrinks and crosses zero at a best unit of 1.4–2.4·10⁶ across
the frames and colours (median 1.6·10⁶), so `U = 1.6·10⁶`. At that unit the coloured fringing of the corrected render
is a small fraction of the uncorrected render's; PR #824's raw-CFA value (3.36·10⁶) sits above the measured range and
under-corrects by about 2×. The unit is a single number for both bodies (as their convention shares everything else);
it scatters by about ±40 % across lenses, so residual fringing remains.

### The control and the default

The Optics panel's **"Remove Chromatic Aberration"** checkbox governs it (Lightroom Classic semantics, independent of
"Enable Lens Corrections"). With it **on**, a render uses the file's table before demosaic when it records a usable
one, and the pipeline's post-demosaic estimator (`estimate_lateral_ca`) as the **fallback** when it does not — never
both: `SourceInfo::raw_lens_ca` says the source already carries the table's correction, and `frame_for` skips the
estimator then. With it **off**, neither runs. The choice is part of the source's identity, so the raw load is told it
(`FileLoader`/`PairLoader` carry it; `DecodedSource::ca_choice` keeps a source developed for the other choice from
standing in), and toggling the checkbox re-develops the file.

Measured on the four frames above (corner R/G, B/G in px at 1440; the switch's own effect):

| Frame | off | on, file has a table | estimator fallback (no table) | on + a forced estimator (control) |
|---|---:|---:|---:|---:|
| DSC03836 | −0.25, +0.26 | −0.02, +0.17 | −0.05, +0.04 | +0.02, +0.02 |
| DSC04440 | +0.03, +0.10 | +0.02, +0.05 | −0.03, +0.03 | −0.03, −0.01 |
| DSC06980 | −0.23, +0.32 | +0.05, −0.05 | −0.03, −0.02 | +0.07, −0.07 |
| DSC02559 | −0.03, +0.25 | −0.01, −0.15 | −0.01, +0.09 | −0.02, −0.09 |

The last column is what the estimator would do if it were **not** gated: run on a render the table already corrected,
it estimates what is *left* (up to 0.17 px, the table's own residual, not near zero) and lays a second, post-demosaic
warp on it — PR #824's 0.032 → 0.052 px case. So it is self-limiting only in that it tracks the residual; it is not
near zero, because the table's unit is a compromise and leaves some. The gate is what keeps the two apart.

The default is **on** (Lightroom Classic checks it by default). That changes existing renders: every raw with a usable
table now gets its CA removed before demosaic without the user asking, and every raw **without** one now runs the
post-demosaic estimator by default where it previously did not (the "estimator fallback" column is that impact — off
−0.25 px → ≈0.04–0.09 px in the corners). It is a render-altering default change, like the two noise changes already
in place in-place.

Four ILCE-7M4 frames on three lenses were used, the corner R/G and B/G CA in px at a 1440 px render:

| Frame (lens) | off R/G, B/G | on (1.6·10⁶) R/G, B/G | camera JPEG R/G, B/G |
|---|---:|---:|---:|
| DSC03836 (Tamron 35–150 @92) | −0.26, +0.26 | −0.02, +0.17 | +0.01, −0.00 |
| DSC04440 (Sigma 70 macro) | +0.03, +0.10 | +0.02, +0.05 | −0.02, +0.03 |
| DSC06980 (Tamron 35–150 @83) | −0.27, +0.33 | +0.05, −0.05 | +0.12, −0.05 |
| DSC02559 (Tamron 35–150 @35) | −0.02, +0.24 | −0.01, −0.15 | −0.01, −0.01 |

Eight of the ten channel-frames move toward the camera JPEG; DSC04589's R/G is the exception (off −0.20, on −0.23), but the
camera JPEG itself measures −0.14 R/G there, so the render's R/G is largely scene colour rather than lens CA and the
estimator's scale is confounded. The distortion warp's registration residual against the camera JPEG is unchanged by the
CA correction (for DSC03836, 0.40 px median off and 0.44 px on with the harness's coarse block matcher), as it should be:
CA is a colour-plane misregistration, not geometry.

## Vignetting: the camera's own JPEG has none to fit

The vignetting table (`0x7032`) is parsed but **not applied**, and the reason is the reference, not the unit. The
camera's own JPEG — the project's validation reference — does not carry a vignetting correction on these files, so
there is nothing in it for a gain model to match.

Measured on the ILCE-7M4 files the earlier sections use, at full resolution (7008 × 4672), against the ARW's own
embedded full-resolution JPEG (`JpgFromRaw`; the user shoots RAW only, so this is the only camera-produced JPEG):

- **It is distortion-corrected.** Block-wise phase correlation of the camera JPEG against the distortion-corrected
  render (this file's `WarpRectilinear` under "Enable Lens Corrections") puts every block at ≤ 1 px over the frame,
  inner and corner; against the *uncorrected* render the same blocks are displaced radially by up to 80 px in the
  corner (DSC03836). So the embedded JPEG shares the corrected geometry, exactly as the distortion section assumes.
- **It is not vignetting-corrected.** The per-radius median ratio of the camera JPEG to the *uncorrected* render
  (scene-linear, midtone band) is flat: 1.00 ± 0.02 from the centre to the corner for DSC03836, and the
  corner-band / centre-band ratio is 1.01 (DSC03836) and 0.99 (DSC04440, Sigma 70 mm). The camera's corner is within
  ~2 % of the uncorrected render's — a gain of ≈ 1, not the table's 1 + 4288/15000 = 1.286.
- **An independent profile-corrected reference does correct it, with this table's convention.** Lightroom Classic's
  export of DSC03836 (already used as a co-reference in the CA section) is a smooth radial gain over the same
  scene: the camera-JPEG / Lightroom-export per-radius median ratio falls monotonically from 0.92 at the centre to
  0.80 at `r/corner = 0.9` and 0.66 at the extreme corner. Removing the global 0.92 tone offset, Lightroom's radial
  gain is ≈ 1.15 at `r/corner = 0.9` and ≈ 1.4 at the extreme corner, against the table's 1.12 and 1.29 under the
  modern convention (knot `i` at `i ·` the full-frame crop half diagonal / 15, a source radius `r` reads
  `1 + v(r)/15000`). The unit is therefore **not** off by a factor as the CA unit was; the earlier "no radial gain
  model fits" is simply that the camera JPEG is flat.

So the table stays read and not applied: applying it would move LightCraft's render *away* from the camera's own JPEG,
which is the reference the project validates against. Testing it against a profile-corrected export is not allowed as
a source of truth (Adobe `.lcp` / profile data is excluded, issue #329), and
[Sony's own documentation](https://www.sony.com/electronics/support/articles/00018031) says lens compensation is
applied to a JPEG shot in RAW+JPEG while the raw stays uncompensated — on these embedded previews the distortion is
baked but the shading is not. A camera JPEG that carries shading compensation (a RAW+JPEG shot, or another body) would
let the same registration-based comparison fit the gain; until one is available the correction stays off, and the UI
says so ("Vignetting — Not applied").

## Older convention: ILCE-7RM4A (Imaging Edge)

LightCraft reads the signed 16-sample distortion table in the raw image IFD (`0x7037`) of
Sony **ILCE-7RM4A (A7R IVA)** Bayer ARWs, and converts it into `OpcodeList3` / `WarpRectilinear`, so the existing
profile correction control, CPU/GPU optics path, EXIF orientation handling, and DNG export use the same correction.

The enabled and independently validated model is ILCE-7RM4A with the FE 24–105mm F4 G OSS and FE 200–600mm F5.6–6.3
G OSS (unit 16384, the table indexed at the output radius). Linear YCbCr ARWs and non-3:2 camera crops are excluded
pending validation. An aspect crop may retain the full-frame radial normalization; the decoder does not guess that
relationship. Older files that carry only encrypted correction metadata, different table lengths, and rejected tables
remain uncorrected.

### Clean-room evidence

The [ExifTool EXIF tag-name documentation](https://exiftool.org/TagNames/EXIF.html) identifies
`0x7037` as 17 signed 16-bit integers. The first value is the count, 16, followed by radial samples.
No external photo decoder implementation or Adobe lens profile was consulted.
Sony Imaging Edge Edit 4.1 was used only as a black-box reference on local scratch copies.

The observed inverse mapping, with radius `r` normalized to the default crop's half diagonal, is:

```
f(r) = 1 + interpolate(table, r) / 16384
source = center + (output - center) * zoom * f(r)
zoom = 1 / max(f(r)) over radii on the rectangular output boundary
```

The border radii range from the shorter half edge / half diagonal to 1. Table knots span 0 through 1.
Linear interpolation approximates the observed map closely; we do not claim to reproduce Sony's
internal interpolator. A fixed-size weighted least-squares fit expresses source radius as
`r * (k0 + k1*r² + k2*r⁴ + k3*r⁶)`. The fit is rejected if radial error exceeds 0.0005 of the
half diagonal (about 0.43 px at a 1440 px 3:2 preview), or the mapping folds. Table values are bounded
and the input must have precisely the supported signed-short layout. Crop offset and normalization
are converted into DNG's active-area coordinate system before creating the opcode. The warp's centre
and normalization come from the standard `DefaultCropOrigin` / `DefaultCropSize` when the file has them,
not from Sony's crop tags: on the tested A7R IVA files the former start at raw x=32 and the latter at
x=0, and the standard origin is the one that aligns with Sony's exports. The image itself keeps the
decoder's crop precedence for every Sony body (Sony's crop tags, then the default crop, then the
recorded image size), so the framing is unchanged; the opcode's centre is in active-area coordinates,
so the warp stays on the optical centre either way. On the A7R IVA that framing still sits 32 raw
pixels left of Sony's exports (eight pixels in a 2376-pixel-wide export).

The main raw-IFD table is important: the similarly named MakerNote table has different values and
must not be substituted into this formula. Main raw-IFD and SR2SubIFD values agreed in the test files.

### Validation

At 2376×1584, independent SIFT correspondences between Sony distortion On and Off TIFF exports gave:

| Lens / focal length | Median residual | 95th percentile | Maximum matched radius |
|---|---:|---:|---:|
| FE 200–600 / 200 mm | 0.043 px | 0.191 px | 0.97 half diagonals |
| FE 200–600 / 600 mm | 0.055 px | 0.231 px | 0.95 half diagonals |

These use the fixed 16384 scale and automatic border framing, not fitted per-image scale or zoom.
A median translation below 0.004 px was removed from the residuals. Correspondences do not cover
all pixels; these are geometric feature residuals, not photometric equality or a worst-case guarantee.
Across 20 sampled RAWs (16 at 24–105 mm, four at 200–600 mm), the polynomial's maximum deviation
from the interpolated file table was below 0.12 px at 1440 px. That measures approximation error,
not agreement with an independent renderer. Camera JPEG and Apple Core Image comparisons support
the wide-lens interpretation but are weaker evidence than same-renderer On/Off pairs.

Historical validation before restoring main's image crop precedence: LightCraft exports with the
standard default crop framing were compared directly with Sony's corrected
TIFFs. At 2376 px wide, median / 95th-percentile feature residuals were 0.35 / 1.04 px at 24 mm,
0.15 / 0.40 px at 200 mm, and 0.13 / 0.56 px at 600 mm (median translation below 0.024 px).
The uncorrected medians at 200 and 600 mm were 8.63 and 10.82 px respectively. Different colour,
sharpening, and demosaicing affect feature localization; these are geometry checks, not colour parity.
An isolated desktop run on Apple M1 Metal detected the profile, rendered without GPU fallback or
warnings, and exposed the existing Optics controls. Existing library edits are not reset: once the
source has loaded, enable lens corrections if they were previously disabled.

After restoring main's crop precedence, fresh 2376×1584 exports on 2026-10-10
(main `b70ae174` plus the aspect-framing guard) measured:

| Focal length | Corrected median / p95 residual | Uncorrected median | Removed translation x / y | Matches / maximum radius |
|---|---|---|---|---|
| 24 mm | 0.36 / 1.12 px | 5.62 px | 7.98 / −0.01 px | 1,442 / 0.95 |
| 200 mm | 0.13 / 0.35 px | 8.65 px | 8.01 / 0.02 px | 11,606 / 0.98 |
| 600 mm | 0.10 / 0.37 px | 10.33 px | 8.02 / 0.00 px | 12,268 / 0.97 |

Residuals subtract only the median feature translation; no scale or lens coefficients are fitted to
these reference pairs. The roughly eight-pixel horizontal translation is the retained crop-origin
difference described above, not a claim of exact Sony framing. Maximum radius is in half diagonals.
The distortion geometry remains aligned while every body's image framing follows main's crop order.

Tests cover signedness/length rejection, zero and malformed tables, independent 600 mm reference
geometry, barrel framing, offset crop normalization, both TIFF byte orders, header/full consistency,
and preservation through DNG export. No private photographs or reference TIFFs are committed.

### Camera coverage and regression boundary (both conventions)

The older decoder's model check is exact: `ILCE-7RM4` (A7R IV without the A suffix), `ILCE-7M3`, unknown/missing
model names, and every other model remain unsupported by it. The modern convention is likewise keyed by exact model
([`BODIES`] in `arw_lens_modern.rs`): `ILCE-7M4` and `ILCE-7RM6`, each only for the knot counts it was measured
with; every other model and layout stays uncorrected. Matching tag numbers and table lengths do not establish the
scale, interpolation or crop normalization on another body. Support can expand after independent geometry
validation; do not add model-name prefix matching.

The current `cargo xtask corpus --download` manifest contains 23 Sony CC0 samples across 17 bodies,
including samples 4822 (A7R IVA / 24–105 mm) and 3989 (A9 II / 200–600 mm). All 23 files have
pinned SHA-256 hashes and are checked with `sony_embedded_distortion_is_limited_to_validated_models`:

| Camera | Corpus samples | Raw-IFD table observed | Expected correction |
|---|---|---|---|
| ILCE-7RM4A | Compressed (4822) | 16 samples | One distortion warp |
| ILCE-9M2 | Compressed (3989) | 16 samples | None |
| ILCE-7M3 | Compressed, uncompressed | 16 samples | None |
| ILCE-7M4 | 14-bit, lossless L/M/S | 11 samples in the 14-bit file; 16 in L/M/S | One warp for the 16-knot full-frame files (lossless L); the 11-knot table stays uncorrected |
| ILCE-7RM2 | 12-bit uncompressed | 16 samples | None |
| DSC-RX100 | One | No raw-IFD table | None |
| DSC-RX100M3 | One | 11 samples | None |
| ILCE-7CR | Compressed, lossless L/M | Not used; model excluded | None |
| ILCE-7RM4 | 14-bit compressed | Not used; model excluded | None |
| DSLR-A500, DSLR-A700, SLT-A33 | One each | Not used; model excluded | None |
| ILCE-3500, ILCE-7S | 5600 K, Shade respectively | Not used; model excluded | None |
| ILCE-7SM2, DSLR-A580 | 16:9 framing, one each | Not used; model excluded | None |
| DSLR-A900 | Packed 12-bit | Not used; model excluded | None |

These are **model-boundary regression checks, not new geometric validation of those models**. In particular,
refusing the A7 III tables resolves the unverified corner warp identified in review. The lossless M/S
files are linear YCbCr and remain excluded independently of the model check. Header and full decode
must agree for every sample. Synthetic tests also verify both TIFF byte orders, exact model matching,
that the warp is centred on the standard default crop while the image keeps Sony's crop, and that DNG export preserves an accepted warp without adding one to rejected
models. A non-3:2 Sony image crop is also rejected when the standard default crop still describes
3:2 geometry; the regression fails without the image-framing guard. The existing independent Sony
geometry tests continue to cover the enabled A7R IVA path.
Before restoring main's crop precedence, re-exporting the 24, 200 and 600 mm reference files after
the model restriction produced byte-identical corrected PNGs to the earlier validated exports. Isolated headless app checks showed the A7R IVA profile
controls present and the A7 III profile controls absent, with no notices and completed RAW renders.

Before enabling another body, compare corrected and uncorrected renders against independent references
across multiple lenses/focal lengths and photographs with features near the edges and corners. Record
residuals and spatial coverage, and check crop/aspect and compression variants. Embedded JPEGs are
useful when the camera actually applied distortion correction; an Off JPEG cannot certify an On warp.
Prefer same-renderer Sony On/Off exports when the camera setting or geometry is ambiguous. Table-fit
error alone only checks our polynomial approximation, not whether the table's interpretation is correct.
