//! Headless tests of the Develop panel's Negative section: the section's text and number formats,
//! the Auto/Manual dark endpoint, every `negative.*` control round-tripping through the panel, and
//! a drag committing live in one undo step.

use std::time::Duration;

use lightcraft_develop::{Section, controls};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn demo(size: [f32; 2], ui: serde_json::Value) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// The Develop panel with only the Negative section open, so every control of the section is on
/// screen at this size.
fn negative_open(size: [f32; 2]) -> Headless {
    let mut h = demo(size, json!({"view": "detail", "right": "edit"}));
    h.app.ui.open_sections = vec!["negative".into()];
    h.step();
    h.step();
    h
}

/// A value on the control's step grid, inside its range, away from its default (the border's auto
/// tail is skipped: its sentinel reads the same text at every negative value).
fn non_default_value(spec: &controls::ControlSpec) -> f64 {
    if spec.id == "negative.border" {
        return 0.075;
    }
    let step = spec.step.max(1e-9);
    for k in [1, -1, 2, -2, 5, -5] {
        let v = spec.clamp(((spec.default + step * k as f64) / step).round() * step);
        if (v - spec.default).abs() > 1e-12 {
            return v;
        }
    }
    (spec.min + spec.max) / 2.0
}

fn settings_of(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn run(h: &mut Headless, command: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    r["result"].clone()
}

/// The section reads like a conversion panel: one switch with the measure action beside it, the
/// profile with its explicit None, a captioned film base at two decimals, an Auto border, a dark
/// endpoint without jargon, and no value label missing its sign or decimals.
#[test]
fn the_negative_section_reads_like_a_conversion_panel() {
    let h = negative_open([1400.0, 2500.0]);
    let painted = h.painted_text();
    for want in [
        "Convert Negative",
        "Measure Film Base",
        "Camera Profile",
        "None",
        "Film Base",
        "measured from the border ring — Auto unless you edit",
        "Border",
        "Auto",
        "Dark Endpoint",
        "Auto (measure)",
        "Manual",
        "Preset",
    ] {
        assert!(painted.iter().any(|t| t == want), "{want} not painted: {painted:?}");
    }
    for gone in ["unfitted", "Border Buffer"] {
        assert!(!painted.iter().any(|t| t.contains(gone)), "{gone} still painted: {painted:?}");
    }
    // the truncated labels the bug produced (the full values contain them as substrings)
    for gone in ["061", "035"] {
        assert!(!painted.iter().any(|t| t == gone), "{gone} still painted: {painted:?}");
    }
    // the manual dark endpoints: sign and four decimals always visible (they read 061 / 035 once)
    for want in ["+0.0281", "+0.0061", "-0.0035"] {
        assert!(painted.iter().any(|t| t == want), "{want} not painted: {painted:?}");
    }
    // the film base reads at two decimals
    for want in ["Base R", "Base G", "Base B"] {
        assert!(painted.iter().any(|t| t == want), "{want} not painted: {painted:?}");
    }
    let d = settings_of(&h);
    for spec in controls::in_section(Section::Negative).filter(|s| s.id.starts_with("negative.base")) {
        let v = controls::get(&d, spec.id).unwrap();
        let want = crate::widgets::shown_value(spec, v);
        assert!(painted.contains(&want), "{want} not painted for {}: {painted:?}", spec.id);
    }
    // every negative control is in the panel
    for spec in controls::in_section(Section::Negative) {
        assert!(h.app.widgets.iter().any(|(w, _)| w.as_str() == format!("slider:{}", spec.id)), "{} is not in the panel", spec.id);
    }
}

/// Auto measures the frame and shows the values read-only; switching to Manual carries the
/// measured endpoint into the sliders, so the conversion keeps its endpoint.
#[test]
fn the_dark_endpoint_switches_between_auto_and_manual_without_losing_the_value() {
    let mut h = negative_open([1400.0, 2500.0]);
    run(&mut h, "develop.negative", json!({"enabled": true}));
    run(&mut h, "develop.negativeDark", json!({"auto": true}));
    h.step();
    h.step();
    let state = run(&mut h, "develop.negativeState", json!({}));
    let measured = state["dark_measured"].as_array().expect("dark_measured").iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!(settings_of(&h).negative.dark_auto, "auto is on");
    let painted = h.painted_text();
    let spec = controls::find("negative.darkR").unwrap();
    for (c, ch) in [(0, "R"), (1, "G"), (2, "B")] {
        let want = crate::widgets::shown_value(spec, measured[c]);
        assert!(painted.contains(&want), "Dark Endpoint {ch} should read {want}: {painted:?}");
        assert!(h.app.widgets.iter().any(|(w, _)| w.as_str() == format!("readout:negativeDark{ch}")), "the measured {ch} is a read-only row");
    }
    // to Manual: the sliders start from what the frame measured
    let r = h.request("ui.clickWidget", json!({"id": "button:negativeDarkMode-manual"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let n = settings_of(&h).negative;
    assert!(!n.dark_auto, "manual now");
    for (c, v) in [(n.dark_r, measured[0]), (n.dark_g, measured[1]), (n.dark_b, measured[2])] {
        assert!((v - c).abs() < 1e-6, "the measured endpoint carried over: {v} vs {c}");
    }
    // and back to Auto
    let r = h.request("ui.clickWidget", json!({"id": "button:negativeDarkMode-auto"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(settings_of(&h).negative.dark_auto, "auto again");
}

/// Every `negative.*` control of the registry: set through `develop.set`, read back equal, and
/// the panel paints the new value and then the default again.
#[test]
fn every_negative_control_round_trips_and_paints() {
    let mut h = negative_open([1400.0, 2500.0]);
    let id = h.app.session.active().unwrap();
    run(&mut h, "develop.negative", json!({"enabled": true}));
    h.step();
    for spec in controls::in_section(Section::Negative) {
        let target = non_default_value(spec);
        run(&mut h, "develop.set", json!({"control": spec.id, "value": target}));
        let d = h.app.session.develop_of(id).unwrap();
        let stored = controls::get(&d, spec.id).unwrap();
        assert!((stored - spec.clamp(target)).abs() < 1e-6, "{}: set {target} read back {stored}", spec.id);
        h.step();
        let want = crate::widgets::shown_value(spec, stored);
        assert!(h.painted_text().contains(&want), "{} should paint {want}", spec.id);
        run(&mut h, "develop.set", json!({"control": spec.id, "value": spec.default}));
        h.step();
        let want = crate::widgets::shown_value(spec, spec.default);
        assert!(h.painted_text().contains(&want), "{} should paint {want} again", spec.id);
    }
}

/// A drag on a film-base slider applies values during the drag and lands as one undo step.
#[test]
fn a_negative_slider_drag_commits_live_in_one_undo_step() {
    let mut h = negative_open([1400.0, 2500.0]);
    let id = h.app.session.active().unwrap();
    run(&mut h, "develop.negative", json!({"enabled": true}));
    h.step();
    let before = h.app.session.catalog.photo(id).unwrap().history.len();
    let start = controls::get(&settings_of(&h), "negative.baseR").unwrap();
    // the drag is pushed as its own events, so frames can be read between them (a ui.dragWidget
    // request plays the whole drag out before it returns)
    let track = h.app.widgets.iter().rev().find(|(w, _)| w == "slider:negative.baseR").map(|(_, r)| *r).expect("the slider");
    let at = egui::pos2(track.left() + track.width() * 0.3, track.center().y);
    let down = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE };
    let up = egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE };
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.app.synthetic.push(down);
    h.app.synthetic.push(egui::Event::PointerMoved(egui::pos2(at.x + 40.0, at.y)));
    h.app.synthetic.push(egui::Event::PointerMoved(egui::pos2(at.x + 80.0, at.y)));
    h.app.synthetic.push(up);
    h.step();
    h.step();
    h.step();
    assert!(h.app.session.interaction.is_some(), "the drag began (beginInteraction ran)");
    let grabbed = controls::get(&settings_of(&h), "negative.baseR").unwrap();
    h.step();
    let mid = controls::get(&settings_of(&h), "negative.baseR").unwrap();
    assert!((mid - grabbed).abs() > 1e-12, "the value follows during the drag: {grabbed} then {mid}");
    assert!((mid - start).abs() > 1e-12, "the drag moved away from the measured base");
    h.step();
    h.step();
    assert!(h.app.session.interaction.is_none(), "the drag ended");
    assert!(h.app.synthetic.is_empty(), "the drag played out");
    let after = h.app.session.catalog.photo(id).unwrap().history.len();
    assert_eq!(after - before, 1, "one undo step for the drag");
    assert_eq!(h.app.session.catalog.photo(id).unwrap().history.last().unwrap().label, "Base R");
    // the slider's readback matches the setting
    let d = settings_of(&h);
    let want = crate::widgets::shown_value(controls::find("negative.baseR").unwrap(), d.negative.base_r);
    assert!(h.painted_text().contains(&want), "{want} not painted");
}

/// The master switch runs the same command the chip did, and the flag on the film base follows
/// the stored base away from (and back to) the frame's measurement.
#[test]
fn the_switch_toggles_the_conversion_and_the_override_flag_follows_the_measurement() {
    let mut h = negative_open([1400.0, 2500.0]);
    assert!(!settings_of(&h).negative.enabled);
    let r = h.request("ui.clickWidget", json!({"id": "switch:negative"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    let d = settings_of(&h);
    assert!(d.negative.enabled, "the switch converted");
    assert!(d.negative.has_base(), "enabling without a base measures one");
    // matching the measurement: not flagged
    let state = run(&mut h, "develop.negativeState", json!({}));
    let m = state["base_measured"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
    assert!((m[0] - d.negative.base_r).abs() < 1e-6, "the stored base is the measured one");
    assert!(!super::panels::edit::base_flag(d.negative.base_r, 0, Some([m[0], m[1], m[2]]), true).1);
    // editing a channel departs from the measurement: flagged, until reset
    run(&mut h, "develop.set", json!({"control": "negative.baseR", "value": 0.5}));
    let d = settings_of(&h);
    let (dimmed, flagged) = super::panels::edit::base_flag(d.negative.base_r, 0, Some([m[0], m[1], m[2]]), true);
    assert!(flagged && !dimmed, "an edited base is flagged: {dimmed} {flagged}");
    // back to the frame's value (the measured one is still on the slider grid): not flagged
    run(&mut h, "develop.set", json!({"control": "negative.baseR", "value": m[0]}));
    let d = settings_of(&h);
    assert!(!super::panels::edit::base_flag(d.negative.base_r, 0, Some([m[0], m[1], m[2]]), true).1);
}
