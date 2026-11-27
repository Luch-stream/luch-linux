//! Where a point of the picture is on this computer. The receiver speaks in fractions of the picture (0..1); the
//! portal wants logical pixels of the stream, an absolute uinput device a fraction of the whole desktop.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The captured area (a monitor, a virtual monitor, a window) inside the compositor's logical layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub area: Rect,
    /// The bounding box of all outputs: what an absolute uinput device spans.
    pub layout: Rect,
}

impl Geometry {
    /// One area that is all there is: a single monitor, or nothing better known.
    pub fn alone(w: f64, h: f64) -> Self {
        let r = Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        };
        Geometry { area: r, layout: r }
    }

    /// From what the portal says of its stream (`position` and `size` are optional there), and the layout when known.
    pub fn from_stream(
        position: Option<(i32, i32)>,
        size: Option<(i32, i32)>,
        video: (u32, u32),
        layout: Option<Rect>,
    ) -> Self {
        let (w, h) = size
            .filter(|s| s.0 > 0 && s.1 > 0)
            .map(|(w, h)| (w as f64, h as f64))
            .unwrap_or((video.0 as f64, video.1 as f64));
        let (x, y) = position
            .map(|(x, y)| (x as f64, y as f64))
            .unwrap_or((0.0, 0.0));
        let area = Rect { x, y, w, h };
        Geometry {
            area,
            layout: layout.unwrap_or(area),
        }
    }

    /// Logical pixels inside the stream, for the portal's Notify*Absolute and NotifyTouch* calls.
    pub fn stream_point(&self, x: f64, y: f64) -> (f64, f64) {
        (clamp(x) * self.area.w, clamp(y) * self.area.h)
    }

    /// A fraction of the whole desktop, for an absolute uinput device (compositors map those to every output).
    pub fn layout_fraction(&self, x: f64, y: f64) -> (f64, f64) {
        let gx = self.area.x + clamp(x) * self.area.w;
        let gy = self.area.y + clamp(y) * self.area.h;
        (
            clamp((gx - self.layout.x) / self.layout.w),
            clamp((gy - self.layout.y) / self.layout.h),
        )
    }

    /// `area` of the grant: what the receiver scales relative motion by.
    pub fn area_size(&self) -> (u32, u32) {
        (self.area.w.round() as u32, self.area.h.round() as u32)
    }
}

fn clamp(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(0.0, 1.0)
    }
}

/// The bounding box of outputs given as (x, y, w, h) in logical pixels.
pub fn bounding(outputs: &[(i32, i32, i32, i32)]) -> Option<Rect> {
    let x0 = outputs.iter().map(|o| o.0).min()?;
    let y0 = outputs.iter().map(|o| o.1).min()?;
    let x1 = outputs.iter().map(|o| o.0 + o.2).max()?;
    let y1 = outputs.iter().map(|o| o.1 + o.3).max()?;
    (x1 > x0 && y1 > y0).then(|| Rect {
        x: x0 as f64,
        y: y0 as f64,
        w: (x1 - x0) as f64,
        h: (y1 - y0) as f64,
    })
}

/// The geometry of output `name` from `niri msg -j outputs` (an object of outputs, each with its `logical`
/// x, y, width, height; null while it is off). niri has no RemoteDesktop portal: uinput needs this.
pub fn niri_geometry(outputs: &serde_json::Value, name: &str) -> Option<Geometry> {
    let rect = |o: &serde_json::Value| {
        let l = &o["logical"];
        Some((
            l["x"].as_i64()? as i32,
            l["y"].as_i64()? as i32,
            l["width"].as_i64()? as i32,
            l["height"].as_i64()? as i32,
        ))
    };
    let all: Vec<_> = outputs.as_object()?.values().filter_map(rect).collect();
    let (x, y, w, h) = rect(outputs.get(name)?)?;
    Some(Geometry {
        area: Rect {
            x: x as f64,
            y: y as f64,
            w: w as f64,
            h: h as f64,
        },
        layout: bounding(&all)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn niri_outputs_give_the_virtual_output_and_the_layout() {
        let v = serde_json::json!({
            "eDP-1": {"name": "eDP-1", "logical": {"x": 0, "y": 0, "width": 1280, "height": 800, "scale": 1.5}},
            "LUCH-192-168-1-20": {"name": "LUCH-192-168-1-20", "logical": {"x": 1280, "y": 0, "width": 1920, "height": 1080, "scale": 1.0}},
            "HDMI-A-1": {"name": "HDMI-A-1", "logical": null}
        });
        let g = niri_geometry(&v, "LUCH-192-168-1-20").unwrap();
        assert_eq!(
            g.area,
            Rect {
                x: 1280.0,
                y: 0.0,
                w: 1920.0,
                h: 1080.0
            }
        );
        assert_eq!(
            g.layout,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 3200.0,
                h: 1080.0
            }
        );
        assert_eq!(g.layout_fraction(0.0, 1.0), (0.4, 1.0));
        assert_eq!(niri_geometry(&v, "HDMI-A-1"), None, "an output that is off");
        assert_eq!(niri_geometry(&v, "DP-9"), None);
    }

    #[test]
    fn stream_points_are_logical_pixels_of_the_stream() {
        // A 4K monitor at scale 1.5: the portal's stream is 2560x1440 logical, the video 3840x2160.
        let g = Geometry::from_stream(Some((0, 0)), Some((2560, 1440)), (3840, 2160), None);
        assert_eq!(g.stream_point(0.0, 0.0), (0.0, 0.0));
        assert_eq!(g.stream_point(0.5, 0.25), (1280.0, 360.0));
        assert_eq!(g.stream_point(1.0, 1.0), (2560.0, 1440.0));
        assert_eq!(
            g.stream_point(-0.2, 7.0),
            (0.0, 1440.0),
            "outside the picture: its edge"
        );
        assert_eq!(g.stream_point(f64::NAN, 0.0), (0.0, 0.0));
        assert_eq!(g.area_size(), (2560, 1440));
    }

    #[test]
    fn without_a_stream_size_the_video_size_stands_in() {
        let g = Geometry::from_stream(None, None, (1920, 1080), None);
        assert_eq!(g.stream_point(1.0, 0.5), (1920.0, 540.0));
        assert_eq!(g.layout_fraction(0.5, 0.5), (0.5, 0.5));
    }

    #[test]
    fn a_monitor_right_of_the_laptop_maps_into_the_whole_desktop() {
        // Laptop 1920x1200 at 0,0; the mirrored (virtual) monitor 1920x1080 right of it at 1920,0.
        let layout = bounding(&[(0, 0, 1920, 1200), (1920, 0, 1920, 1080)]).unwrap();
        assert_eq!(
            layout,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 3840.0,
                h: 1200.0
            }
        );
        let g = Geometry::from_stream(
            Some((1920, 0)),
            Some((1920, 1080)),
            (1920, 1080),
            Some(layout),
        );
        assert_eq!(g.layout_fraction(0.0, 0.0), (0.5, 0.0));
        assert_eq!(g.layout_fraction(1.0, 1.0), (1.0, 0.9));
        assert_eq!(g.layout_fraction(0.5, 0.5), (0.75, 0.45));
        // The portal still wants stream-local pixels for the same point.
        assert_eq!(g.stream_point(0.5, 0.5), (960.0, 540.0));
    }

    #[test]
    fn outputs_left_of_and_above_zero() {
        let layout = bounding(&[(-1280, -200, 1280, 1024), (0, 0, 2560, 1440)]).unwrap();
        assert_eq!(
            layout,
            Rect {
                x: -1280.0,
                y: -200.0,
                w: 3840.0,
                h: 1640.0
            }
        );
        let g = Geometry::from_stream(
            Some((-1280, -200)),
            Some((1280, 1024)),
            (1280, 1024),
            Some(layout),
        );
        assert_eq!(g.layout_fraction(0.0, 0.0), (0.0, 0.0));
        let (fx, fy) = g.layout_fraction(1.0, 1.0);
        assert!((fx - 1280.0 / 3840.0).abs() < 1e-12 && (fy - 1024.0 / 1640.0).abs() < 1e-12);
        assert_eq!(bounding(&[]), None);
    }
}
