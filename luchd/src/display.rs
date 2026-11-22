//! Receiver display dimensions and temporary GNOME layout for extended desktops.
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use zbus::{zvariant::OwnedValue, Connection};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}
impl Default for Size {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
        }
    }
}
impl Size {
    fn new(width: u64, height: u64) -> Option<Self> {
        // NV12 requires even dimensions. Do not accept unbounded dimensions from the network.
        (16..=8192).contains(&width).then_some(())?;
        (16..=8192).contains(&height).then_some(())?;
        Some(Self {
            width: width as u32 & !1,
            height: height as u32 & !1,
        })
    }
}

/// Use display capabilities, never the decoder's unrelated maximum coded frame size.
/// Browsers send physical screen dimensions; older Android receivers advertise physical modes.
pub fn native_size(welcome: &Value) -> Option<Size> {
    let display = &welcome["hw"]["display"];
    if let Some(size) = display["width"]
        .as_u64()
        .zip(display["height"].as_u64())
        .and_then(|(w, h)| Size::new(w, h))
    {
        return Some(size);
    }
    display["modes"]
        .as_array()?
        .iter()
        .filter_map(|mode| {
            let resolution = mode.as_str()?.split('@').next()?;
            let (w, h) = resolution.split_once('x')?;
            Size::new(w.parse().ok()?, h.parse().ok()?)
        })
        .max_by_key(|s| u64::from(s.width) * u64::from(s.height))
}

type Props = HashMap<String, OwnedValue>;
type Spec = (String, String, String, String);
type Mode = (String, i32, i32, f64, f64, Vec<f64>, Props);
type Monitor = (Spec, Vec<Mode>, Props);
type Logical = (i32, i32, f64, u32, bool, Vec<Spec>, Props);
type State = (u32, Vec<Monitor>, Vec<Logical>, Props);
type Applied = (i32, i32, f64, u32, bool, Vec<(String, String, Props)>);
const NAME: &str = "org.gnome.Mutter.DisplayConfig";
const PATH: &str = "/org/gnome/Mutter/DisplayConfig";
fn flag(props: &Props, key: &str) -> bool {
    props
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}
fn number(props: &Props, key: &str) -> Option<u32> {
    props.get(key).and_then(|v| u32::try_from(v).ok())
}
fn current(monitor: &Monitor) -> Option<&Mode> {
    monitor.1.iter().find(|m| flag(&m.6, "is-current"))
}
async fn state(conn: &Connection) -> Result<State, String> {
    conn.call_method(Some(NAME), PATH, Some(NAME), "GetCurrentState", &())
        .await
        .map_err(|e| format!("GNOME display layout: {e}"))?
        .body()
        .deserialize()
        .map_err(|e| e.to_string())
}

pub struct Layout {
    conn: Connection,
    before: HashSet<String>,
}
impl Layout {
    pub async fn capture() -> Result<Self, String> {
        let conn = Connection::session().await.map_err(|e| e.to_string())?;
        let before = state(&conn).await?.1.into_iter().map(|m| m.0 .0).collect();
        Ok(Self { conn, before })
    }
    pub async fn place_new_right(&self, size: Size) -> Result<String, String> {
        let mut last_error = "virtual monitor did not appear".to_string();
        // PipeWire creates/resizes the monitor asynchronously. A changed layout serial also
        // needs a fresh snapshot; never reapply an old configuration after another change.
        for _ in 0..30 {
            let mut state = state(&self.conn).await?;
            let new: Vec<_> = state
                .1
                .iter()
                .filter(|m| !self.before.contains(&m.0 .0) && m.0 .2 == "Virtual remote monitor")
                .collect();
            if new.len() > 1 {
                return Err("cannot identify our virtual monitor unambiguously".into());
            }
            if let Some(monitor) = new.first() {
                if current(monitor)
                    .is_some_and(|m| m.1 == size.width as i32 && m.2 == size.height as i32)
                {
                    let connector = monitor.0 .0.clone();
                    match place_right(&mut state, &connector) {
                        Ok(None) => return Ok(connector), // GNOME already placed it on the right.
                        Ok(Some(logical)) => {
                            let mut props = Props::new();
                            if flag(&state.3, "supports-changing-layout-mode") {
                                if let Some(mode) = number(&state.3, "layout-mode") {
                                    props.insert("layout-mode".into(), mode.into());
                                }
                            }
                            let leased: Vec<_> = state
                                .1
                                .iter()
                                .filter(|m| flag(&m.2, "is-for-lease"))
                                .map(|m| m.0.clone())
                                .collect();
                            if !leased.is_empty() {
                                props.insert(
                                    "monitors-for-lease".into(),
                                    OwnedValue::try_from(zbus::zvariant::Value::new(leased))
                                        .map_err(|e| e.to_string())?,
                                );
                            }
                            match self
                                .conn
                                .call_method(
                                    Some(NAME),
                                    PATH,
                                    Some(NAME),
                                    "ApplyMonitorsConfig",
                                    &(state.0, 1u32, logical, props),
                                )
                                .await
                            {
                                Ok(_) => return Ok(connector), // temporary: do not overwrite monitors.xml
                                Err(e) => last_error = e.to_string(),
                            }
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Err(format!("virtual display layout: {last_error}"))
    }
}

/// Keep every existing mode, transform, scale and primary flag; move only our new connector.
fn place_right(state: &mut State, connector: &str) -> Result<Option<Vec<Applied>>, String> {
    let logical_mode = number(&state.3, "layout-mode").unwrap_or(1) == 1;
    let mut anchor = None;
    let mut target = None;
    for (index, logical) in state.2.iter().enumerate() {
        if logical.5.iter().any(|s| s.0 == connector) {
            if logical.5.len() != 1 {
                return Err("virtual monitor unexpectedly belongs to a mirrored group".into());
            }
            target = Some(index);
            continue;
        }
        let spec = logical.5.first().ok_or("empty logical monitor")?;
        let monitor = state
            .1
            .iter()
            .find(|m| &m.0 == spec)
            .ok_or("missing monitor")?;
        let mode = current(monitor).ok_or("missing active monitor mode")?;
        let pixels = if logical.3 % 2 == 1 { mode.2 } else { mode.1 };
        let width = if logical_mode {
            (f64::from(pixels) / logical.2).round() as i32
        } else {
            pixels
        };
        let edge = logical.0 + width;
        if anchor.is_none_or(|(right, _, _)| edge > right) {
            anchor = Some((edge, logical.1, logical.4));
        }
    }
    let index = target.ok_or("virtual monitor has no logical monitor")?;
    let Some((x, y, _)) = anchor else {
        return Ok(None);
    }; // only display: nothing to extend
    let target = &mut state.2[index];
    if target.0 == x && target.1 == y {
        return Ok(None);
    }
    target.0 = x;
    target.1 = y;
    let mut applied = Vec::new();
    for logical in &state.2 {
        let mut monitors = Vec::new();
        for spec in &logical.5 {
            let monitor = state
                .1
                .iter()
                .find(|m| &m.0 == spec)
                .ok_or("missing monitor")?;
            let mode = current(monitor).ok_or("missing active monitor mode")?;
            let mut props = Props::new();
            for (source, target) in [
                ("is-underscanning", "underscanning"),
                ("color-mode", "color-mode"),
                ("rgb-range", "rgb-range"),
            ] {
                if let Some(value) = monitor.2.get(source) {
                    props.insert(target.into(), value.try_clone().map_err(|e| e.to_string())?);
                }
            }
            monitors.push((spec.0.clone(), mode.0.clone(), props));
        }
        applied.push((
            logical.0, logical.1, logical.2, logical.3, logical.4, monitors,
        ));
    }
    Ok(Some(applied))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn uses_display_modes_instead_of_decoder_limit() {
        let welcome =
            json!({"max":"4096x2304", "hw":{"display":{"modes":["1920x1080@120","3840x2160@60"]}}});
        assert_eq!(
            native_size(&welcome),
            Some(Size {
                width: 3840,
                height: 2160
            })
        );
        assert_eq!(native_size(&json!({"max":"4096x2304"})), None);
    }
    #[test]
    fn browser_dimensions_preserve_orientation_and_reject_bad_sizes() {
        assert_eq!(
            native_size(&json!({"hw":{"display":{"width":1179,"height":2556}}})),
            Some(Size {
                width: 1178,
                height: 2556
            })
        );
        for modes in [
            json!(["0x0@60"]),
            json!(["999999x1080@60"]),
            json!(["garbage"]),
        ] {
            assert_eq!(
                native_size(&json!({"hw":{"display":{"modes":modes}}})),
                None
            );
        }
    }
    fn monitor(name: &str, width: i32, height: i32) -> Monitor {
        let spec = (
            name.into(),
            "Vendor".into(),
            "Panel".into(),
            "serial".into(),
        );
        let mut props = Props::new();
        props.insert("is-current".into(), true.into());
        let mode = (
            format!("{width}x{height}@60"),
            width,
            height,
            60.0,
            1.0,
            vec![1.0, 1.25, 1.5],
            props,
        );
        (spec, vec![mode], Props::new())
    }
    #[test]
    fn places_virtual_right_of_scaled_rotated_displays_without_changing_them() {
        let mut main = monitor("main", 2560, 1440);
        main.2.insert("is-underscanning".into(), true.into());
        main.2.insert("color-mode".into(), 2u32.into());
        let rotated = monitor("portrait", 1920, 1080);
        let virtual_monitor = monitor("virtual", 3840, 2160);
        let logical = vec![
            (0, 0, 1.25, 0, true, vec![main.0.clone()], Props::new()),
            (
                2048,
                100,
                1.5,
                1,
                false,
                vec![rotated.0.clone()],
                Props::new(),
            ),
            (
                0,
                2000,
                1.0,
                0,
                false,
                vec![virtual_monitor.0.clone()],
                Props::new(),
            ),
        ];
        let mut state = (
            1,
            vec![main, rotated, virtual_monitor],
            logical,
            Props::new(),
        );
        let applied = place_right(&mut state, "virtual").unwrap().unwrap();
        assert_eq!((applied[2].0, applied[2].1), (2768, 100));
        assert_eq!(
            (
                applied[0].0,
                applied[0].1,
                applied[0].2,
                applied[0].3,
                applied[0].4
            ),
            (0, 0, 1.25, 0, true)
        );
        assert_eq!(
            (applied[1].0, applied[1].1, applied[1].2, applied[1].3),
            (2048, 100, 1.5, 1)
        );
        assert!(flag(&applied[0].5[0].2, "underscanning"));
        assert_eq!(number(&applied[0].5[0].2, "color-mode"), Some(2));
        assert!(place_right(&mut state, "virtual").unwrap().is_none());
    }
    #[test]
    fn physical_layout_uses_pixel_width_without_dividing_by_scale() {
        let main = monitor("main", 3840, 2160);
        let target = monitor("virtual", 1920, 1080);
        let logical = vec![
            (0, 0, 2.0, 0, true, vec![main.0.clone()], Props::new()),
            (0, 2160, 1.0, 0, false, vec![target.0.clone()], Props::new()),
        ];
        let mut props = Props::new();
        props.insert("layout-mode".into(), 2u32.into());
        let mut state = (1, vec![main, target], logical, props);
        let applied = place_right(&mut state, "virtual").unwrap().unwrap();
        assert_eq!((applied[1].0, applied[1].1), (3840, 0));
    }
}
