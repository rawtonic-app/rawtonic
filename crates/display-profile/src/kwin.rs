//! What KWin (KDE Plasma 6 on Wayland) does with a display's colours, read
//! from its output configuration (`kwinoutputconfig.json`, written by
//! System Settings > Display & Monitor). KWin 6 composites every window
//! into the output's colour space: with a colour profile source other than
//! sRGB (an ICC profile, the display's EDID) or HDR / wide colour gamut on,
//! it converts an sRGB window into the display itself, so the app must not
//! convert it again. Pure parsing: tested on every system.

use serde_json::Value;

/// One output as KWin's configuration describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KwinOutput {
    /// The connector (`DP-1`, `HDMI-A-1`): colord's `XRANDR_name`.
    pub connector: String,
    /// Whether KWin converts sRGB windows into this display itself.
    pub converts: bool,
    /// What it converts with, for Preferences (`ICC /path`, `EDID`, `HDR`).
    pub source: String,
}

fn output_of(data: &Value) -> Option<KwinOutput> {
    let connector = data.get("connectorName")?.as_str()?.to_string();
    let flag = |key: &str| data.get(key).and_then(Value::as_bool).unwrap_or(false);
    let source = data
        .get("colorProfileSource")
        .and_then(Value::as_str)
        .unwrap_or("sRGB");
    let icc = data
        .get("iccProfilePath")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (converts, source) = if flag("highDynamicRange") {
        (true, "HDR".to_string())
    } else if flag("wideColorGamut") {
        (true, "wide colour gamut".to_string())
    } else {
        match source {
            "ICC" if !icc.is_empty() => (true, format!("ICC {icc}")),
            "EDID" => (true, "EDID".to_string()),
            other => (false, other.to_string()),
        }
    };
    Some(KwinOutput {
        connector,
        converts,
        source,
    })
}

/// A layout's enabled outputs: index into the outputs, logical position.
type Setup = Vec<(usize, (i64, i64))>;

/// A monitor as the desktop shows it: logical position and size.
pub type Monitor = ((i32, i32), (i32, i32));

/// The outputs of `config` and its layouts ("setups").
fn parse(config: &str) -> Option<(Vec<Value>, Vec<Setup>)> {
    let root: Value = serde_json::from_str(config).ok()?;
    let section = |name: &str| {
        root.as_array()?
            .iter()
            .find(|s| s.get("name").and_then(Value::as_str) == Some(name))?
            .get("data")?
            .as_array()
            .cloned()
    };
    let outputs = section("outputs")?;
    let setups = section("setups")
        .unwrap_or_default()
        .iter()
        .map(|setup| {
            setup
                .get("outputs")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|o| o.get("enabled").and_then(Value::as_bool).unwrap_or(false))
                .filter_map(|o| {
                    let index = o.get("outputIndex")?.as_u64()? as usize;
                    let at = o.get("position")?;
                    Some((index, (at.get("x")?.as_i64()?, at.get("y")?.as_i64()?)))
                })
                .collect()
        })
        .collect();
    Some((outputs, setups))
}

/// An output's logical size (its mode divided by its scale; rotated
/// outputs either way round), if the configuration says.
fn logical_size(output: &Value) -> Option<(f64, f64)> {
    // KWin 6.5 writes `mode.width`/`height`, later ones a `cvt` timing.
    let mode = output.get("mode")?;
    let scale = output.get("scale").and_then(Value::as_f64).unwrap_or(1.0);
    let (w, h) = match mode.get("cvt") {
        Some(cvt) => (
            cvt.get("hdisplay")?.as_f64()?,
            cvt.get("vdisplay")?.as_f64()?,
        ),
        None => (mode.get("width")?.as_f64()?, mode.get("height")?.as_f64()?),
    };
    Some((w / scale, h / scale))
}

/// Whether `setup` shows exactly `monitors`: the same positions, each
/// output of the size the desktop gives the monitor there (±2 px).
fn fits(outputs: &[Value], setup: &Setup, monitors: &[Monitor]) -> bool {
    setup.len() == monitors.len()
        && monitors.iter().all(|&((x, y), (w, h))| {
            setup.iter().any(|(index, at)| {
                *at == (x as i64, y as i64)
                    && outputs
                        .get(*index)
                        .and_then(logical_size)
                        .is_none_or(|(ow, oh)| {
                            let near = |a: f64, b: i32| (a - b as f64).abs() <= 2.0;
                            (near(ow, w) && near(oh, h)) || (near(ow, h) && near(oh, w))
                        })
            })
        })
}

/// The output at logical `position` when the desktop shows `monitors`: the
/// layouts of KWin's that fit them must agree on it. `None` when none fits
/// or they disagree (the window's output is then unknown).
pub fn output_at(config: &str, monitors: &[Monitor], position: (i32, i32)) -> Option<KwinOutput> {
    let (outputs, setups) = parse(config)?;
    let at = (position.0 as i64, position.1 as i64);
    let mut found: Option<usize> = None;
    for setup in setups.iter().filter(|s| fits(&outputs, s, monitors)) {
        let Some((index, _)) = setup.iter().find(|(_, p)| *p == at) else {
            continue;
        };
        let connector = |i: usize| outputs.get(i).and_then(output_of).map(|o| o.connector);
        match found {
            Some(previous) if connector(previous) != connector(*index) => return None,
            _ => found = Some(*index),
        }
    }
    outputs.get(found?).and_then(output_of)
}

/// An output KWin converts sRGB windows into itself among the layouts that
/// fit `monitors` (used when the window's output is unknown: converting
/// there could be converting twice). Displays of other layouts (one
/// undocked, say) do not count.
pub fn layout_output_converts(config: &str, monitors: &[Monitor]) -> Option<KwinOutput> {
    let (outputs, setups) = parse(config)?;
    setups
        .iter()
        .filter(|s| fits(&outputs, s, monitors))
        .flatten()
        .filter_map(|(index, _)| outputs.get(*index).and_then(output_of))
        .find(|o| o.converts)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DP-1 4K at 1.45 (2649 × 1490), HDMI-A-1 1920 × 1200, eDP-1 a laptop
    /// panel 1920 × 1080; layouts: DP-1 + HDMI-A-1, DP-1 alone, eDP-1
    /// alone (undocked), HDMI-A-1 + eDP-1.
    fn config(dp: &str, hdmi: &str) -> String {
        let mode =
            |w: u32, h: u32| format!(r#""mode":{{"cvt":{{"hdisplay":{w},"vdisplay":{h}}}}}"#);
        format!(
            r#"[{{"name":"outputs","data":[
                {{"connectorName":"DP-1","colorProfileSource":{dp},"highDynamicRange":false,"scale":1.45,{}}},
                {{"connectorName":"HDMI-A-1","colorProfileSource":{hdmi},"highDynamicRange":false,"scale":1,{}}},
                {{"connectorName":"eDP-1","colorProfileSource":"sRGB","scale":1,{}}}]}},
              {{"name":"setups","data":[
                {{"outputs":[{{"enabled":true,"outputIndex":0,"position":{{"x":0,"y":0}}}},
                             {{"enabled":true,"outputIndex":1,"position":{{"x":2649,"y":0}}}}]}},
                {{"outputs":[{{"enabled":true,"outputIndex":0,"position":{{"x":0,"y":0}}}}]}},
                {{"outputs":[{{"enabled":true,"outputIndex":2,"position":{{"x":0,"y":0}}}}]}},
                {{"outputs":[{{"enabled":true,"outputIndex":1,"position":{{"x":0,"y":0}}}},
                             {{"enabled":true,"outputIndex":2,"position":{{"x":1920,"y":0}}}}]}}]}}]"#,
            mode(3840, 2160),
            mode(1920, 1200),
            mode(1920, 1080)
        )
    }

    const TWO: [Monitor; 2] = [((0, 0), (2649, 1490)), ((2649, 0), (1920, 1200))];

    #[test]
    fn the_window_output_is_found_by_the_layout() {
        let c = config(r#""sRGB""#, r#""ICC","iccProfilePath":"/p/wide.icc""#);
        let first = output_at(&c, &TWO, (0, 0)).unwrap();
        assert_eq!((first.connector.as_str(), first.converts), ("DP-1", false));
        let second = output_at(&c, &TWO, (2649, 0)).unwrap();
        assert_eq!(second.connector, "HDMI-A-1");
        assert!(second.converts);
        assert_eq!(second.source, "ICC /p/wide.icc");
        // The other two-display layout: (0, 0) is HDMI-A-1 there.
        let other = [((0, 0), (1920, 1200)), ((1920, 0), (1920, 1080))];
        assert_eq!(output_at(&c, &other, (0, 0)).unwrap().connector, "HDMI-A-1");
        // A layout KWin does not know: unknown, never a guess.
        assert_eq!(
            output_at(
                &c,
                &[((0, 0), (2649, 1490)), ((100, 0), (800, 600))],
                (0, 0)
            ),
            None
        );
        assert_eq!(
            layout_output_converts(&c, &TWO).unwrap().connector,
            "HDMI-A-1"
        );
    }

    #[test]
    fn undocked_the_panel_is_told_from_the_external_display_by_its_size() {
        // The external DP-1 managed by KWin (ICC); undocked, the panel
        // alone at (0, 0) as DP-1 was alone: not managed, not mistaken.
        let c = config(r#""ICC","iccProfilePath":"/p/wide.icc""#, r#""sRGB""#);
        let panel = [((0, 0), (1920, 1080))];
        let alone = output_at(&c, &panel, (0, 0)).unwrap();
        assert_eq!((alone.connector.as_str(), alone.converts), ("eDP-1", false));
        assert_eq!(layout_output_converts(&c, &panel), None);
        let external = [((0, 0), (2649, 1490))];
        assert!(output_at(&c, &external, (0, 0)).unwrap().converts);
    }

    #[test]
    fn kwin_6_5_mode_sizes_are_read_too() {
        // Plasma 6.5 writes the mode as width/height.
        let c = config(r#""ICC","iccProfilePath":"/p/wide.icc""#, r#""sRGB""#)
            .replace(
                r#""cvt":{"hdisplay":3840,"vdisplay":2160}"#,
                r#""width":3840,"height":2160,"refreshRate":60000"#,
            )
            .replace(
                r#""cvt":{"hdisplay":1920,"vdisplay":1080}"#,
                r#""width":1920,"height":1080,"refreshRate":60000"#,
            );
        assert!(c.contains(r#""width":1920"#) && !c.contains("hdisplay\":1920,\"vdisplay\":1080"));
        let panel = [((0, 0), (1920, 1080))];
        assert_eq!(output_at(&c, &panel, (0, 0)).unwrap().connector, "eDP-1");
        assert_eq!(layout_output_converts(&c, &panel), None);
        assert!(
            output_at(&c, &[((0, 0), (2649, 1490))], (0, 0))
                .unwrap()
                .converts
        );
    }

    #[test]
    fn edid_and_hdr_are_colour_managed_srgb_and_an_empty_icc_are_not() {
        let c = config(r#""EDID""#, r#""ICC","iccProfilePath":"""#);
        assert!(output_at(&c, &TWO, (0, 0)).unwrap().converts);
        assert!(!output_at(&c, &TWO, (2649, 0)).unwrap().converts);
        let hdr = c.replacen(
            r#""highDynamicRange":false"#,
            r#""highDynamicRange":true"#,
            2,
        );
        assert_eq!(output_at(&hdr, &TWO, (2649, 0)).unwrap().source, "HDR");
        assert_eq!(
            layout_output_converts(&config(r#""sRGB""#, r#""sRGB""#), &TWO),
            None
        );
        assert_eq!(output_at("not json", &TWO, (0, 0)), None);
    }
}
