//! A plane seen in perspective: the homography that takes pattern units to image pixels.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
}

pub const fn pt(x: f64, y: f64) -> Pt {
    Pt { x, y }
}

#[derive(Clone, Copy, Debug)]
pub struct Homography([f64; 9]);

impl Homography {
    /// The map taking `from[i]` to `to[i]` (four points, no three on a line).
    pub fn new(from: [Pt; 4], to: [Pt; 4]) -> Option<Homography> {
        // h33 = 1; two equations per point, eight unknowns.
        let mut m = [[0.0f64; 9]; 8];
        for i in 0..4 {
            let (x, y, u, v) = (from[i].x, from[i].y, to[i].x, to[i].y);
            m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        for c in 0..8 {
            let p = (c..8).max_by(|&a, &b| m[a][c].abs().total_cmp(&m[b][c].abs()))?;
            if m[p][c].abs() < 1e-12 {
                return None;
            }
            m.swap(c, p);
            let pivot = m[c];
            for (r, row) in m.iter_mut().enumerate() {
                if r != c {
                    let k = row[c] / pivot[c];
                    row.iter_mut()
                        .zip(pivot)
                        .skip(c)
                        .for_each(|(v, p)| *v -= k * p);
                }
            }
        }
        let mut h = [1.0; 9];
        for (i, row) in m.iter().enumerate() {
            h[i] = row[8] / row[i];
        }
        Some(Homography(h))
    }

    /// The unit square (pattern units of a QR code) onto four corners: top-left, top-right, bottom-right, bottom-left.
    pub fn from_unit_square(corners: [Pt; 4]) -> Option<Homography> {
        Homography::new(
            [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)],
            corners,
        )
    }

    pub fn map(&self, p: Pt) -> Pt {
        let h = &self.0;
        let w = h[6] * p.x + h[7] * p.y + h[8];
        pt(
            (h[0] * p.x + h[1] * p.y + h[2]) / w,
            (h[3] * p.x + h[4] * p.y + h[5]) / w,
        )
    }

    pub fn inverse(&self) -> Option<Homography> {
        let h = &self.0;
        let det = h[0] * (h[4] * h[8] - h[5] * h[7]) - h[1] * (h[3] * h[8] - h[5] * h[6])
            + h[2] * (h[3] * h[7] - h[4] * h[6]);
        if det.abs() < 1e-15 {
            return None;
        }
        let i = [
            h[4] * h[8] - h[5] * h[7],
            h[2] * h[7] - h[1] * h[8],
            h[1] * h[5] - h[2] * h[4],
            h[5] * h[6] - h[3] * h[8],
            h[0] * h[8] - h[2] * h[6],
            h[2] * h[3] - h[0] * h[5],
            h[3] * h[7] - h[4] * h[6],
            h[1] * h[6] - h[0] * h[7],
            h[0] * h[4] - h[1] * h[3],
        ];
        Some(Homography(i.map(|v| v / det)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_corners_and_inverts() {
        let c = [
            pt(100.0, 50.0),
            pt(300.0, 70.0),
            pt(280.0, 260.0),
            pt(90.0, 240.0),
        ];
        let h = Homography::from_unit_square(c).unwrap();
        for (u, p) in [pt(0.0, 0.0), pt(1.0, 0.0), pt(1.0, 1.0), pt(0.0, 1.0)]
            .iter()
            .zip(c)
        {
            let q = h.map(*u);
            assert!((q.x - p.x).abs() < 1e-9 && (q.y - p.y).abs() < 1e-9);
        }
        let back = h.inverse().unwrap().map(h.map(pt(2.5, 1.4)));
        assert!((back.x - 2.5).abs() < 1e-9 && (back.y - 1.4).abs() < 1e-9);
    }
}
