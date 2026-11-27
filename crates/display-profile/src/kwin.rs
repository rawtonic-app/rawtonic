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

/// The output at logical `position` when the desktop shows `monitors`
/// (every monitor's logical position): the one layout of KWin's whose
/// enabled outputs sit exactly there. `None` when no layout or several
/// differing ones fit (the window's output is then unknown).
pub fn output_at(
    config: &str,
    monitors: &[(i32, i32)],
    position: (i32, i32),
) -> Option<KwinOutput> {
    let (outputs, setups) = parse(config)?;
    let mut wanted: Vec<(i64, i64)> = monitors
        .iter()
        .map(|&(x, y)| (x as i64, y as i64))
        .collect();
    wanted.sort_unstable();
    let at = (position.0 as i64, position.1 as i64);
    let mut found: Option<usize> = None;
    for setup in setups {
        let mut positions: Vec<(i64, i64)> = setup.iter().map(|(_, p)| *p).collect();
        positions.sort_unstable();
        if positions != wanted {
            continue;
        }
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

/// Whether KWin converts sRGB windows into any configured output itself
/// (used when the window's output is unknown: converting there could be
/// converting twice).
pub fn any_output_converts(config: &str) -> Option<KwinOutput> {
    let (outputs, _) = parse(config)?;
    outputs.iter().filter_map(output_of).find(|o| o.converts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(first: &str, second: &str) -> String {
        format!(
            r#"[{{"name":"outputs","data":[
                {{"connectorName":"DP-1","colorProfileSource":{first},"highDynamicRange":false}},
                {{"connectorName":"HDMI-A-1","colorProfileSource":{second},"highDynamicRange":false}},
                {{"connectorName":"DP-2","colorProfileSource":"sRGB"}}]}},
              {{"name":"setups","data":[
                {{"outputs":[{{"enabled":true,"outputIndex":0,"position":{{"x":0,"y":0}}}},
                             {{"enabled":true,"outputIndex":1,"position":{{"x":2649,"y":0}}}}]}},
                {{"outputs":[{{"enabled":true,"outputIndex":0,"position":{{"x":0,"y":0}}}}]}},
                {{"outputs":[{{"enabled":true,"outputIndex":1,"position":{{"x":0,"y":0}}}},
                             {{"enabled":true,"outputIndex":2,"position":{{"x":1920,"y":0}}}}]}}]}}]"#
        )
    }

    #[test]
    fn the_window_output_is_found_by_the_layout() {
        let c = config(r#""sRGB""#, r#""ICC","iccProfilePath":"/p/wide.icc""#);
        let two = [(0, 0), (2649, 0)];
        let first = output_at(&c, &two, (0, 0)).unwrap();
        assert_eq!((first.connector.as_str(), first.converts), ("DP-1", false));
        let second = output_at(&c, &two, (2649, 0)).unwrap();
        assert_eq!(second.connector, "HDMI-A-1");
        assert!(second.converts);
        assert_eq!(second.source, "ICC /p/wide.icc");
        // The other two-display layout: (0, 0) is HDMI-A-1 there.
        assert_eq!(
            output_at(&c, &[(0, 0), (1920, 0)], (0, 0))
                .unwrap()
                .connector,
            "HDMI-A-1"
        );
        assert_eq!(output_at(&c, &[(0, 0)], (0, 0)).unwrap().connector, "DP-1");
        // A layout KWin does not know: unknown, never a guess.
        assert_eq!(output_at(&c, &[(0, 0), (100, 0)], (0, 0)), None);
        assert_eq!(any_output_converts(&c).unwrap().connector, "HDMI-A-1");
    }

    #[test]
    fn edid_and_hdr_are_colour_managed_srgb_and_an_empty_icc_are_not() {
        let c = config(r#""EDID""#, r#""ICC","iccProfilePath":"""#);
        let two = [(0, 0), (2649, 0)];
        assert!(output_at(&c, &two, (0, 0)).unwrap().converts);
        assert!(!output_at(&c, &two, (2649, 0)).unwrap().converts);
        let hdr = c.replacen(
            r#""highDynamicRange":false"#,
            r#""highDynamicRange":true"#,
            2,
        );
        assert_eq!(output_at(&hdr, &two, (2649, 0)).unwrap().source, "HDR");
        assert_eq!(any_output_converts(&config(r#""sRGB""#, r#""sRGB""#)), None);
        assert_eq!(output_at("not json", &two, (0, 0)), None);
    }
}
