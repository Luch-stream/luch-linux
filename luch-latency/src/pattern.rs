//! The test pattern both ends draw: a QR code with what the end knows about itself, over two rows of Gray-code cells
//! that count its frames.
//!
//! Everything is measured in units of the QR code's side, with the QR code's top-left corner at the origin. A reader
//! that finds the QR code (its four corners) therefore knows where every cell is, at any angle and in any
//! perspective. The receiver draws the same layout (PROTOCOL.md, section 4h), so this file is the reference.
//!
//! ```text
//!  (-0.15,-0.15) ┌──────────────┐ white box
//!                │ ┌──────────┐ │
//!                │ │    QR    │ │
//!                │ │          │ │
//!                │ └──────────┘ │
//!                │ □■■□■□□      │ row 0: white, black, bits 11..7
//!                │ ■□□■■■□      │ row 1: bits 6..0
//!                └──────────────┘ (1.55, 2.2)
//! ```
//!
//! Why Gray code: from one number to the next exactly one cell changes. A camera frame taken while the screen changes
//! (motion blur, a slow panel, a tear line, rolling shutter) sees at most that one cell half-way, and any mix of two
//! neighbouring codes is one of the two, so it reads either the old number or the new one, never a third. The cells
//! are short and wide, so that a tear or the camera's rolling shutter, which both run along rows, cross a row at once.
//! The big counter for people is drawn beside the box and is not part of the layout.

/// The white box around the pattern: left, top, right, bottom.
pub const BOX: [f64; 4] = [-0.15, -0.15, 1.55, 2.2];
/// Top and bottom of each row of cells.
pub const ROWS: [[f64; 2]; 2] = [[1.15, 1.55], [1.65, 2.05]];
/// The middle of the pattern's cells, between the rows: where its time is measured.
pub const BETWEEN_ROWS: f64 = (ROWS[0][1] + ROWS[1][0]) / 2.0;
/// Cells per row and the width of one.
pub const PER_ROW: usize = 7;
pub const CELL_W: f64 = 0.2;
/// Gray-code bits of the counter: it wraps after 4096 frames (68 s at 60 Hz), far longer than any latency.
pub const BITS: u32 = 12;
/// Cell 0 is always white and cell 1 always black: the reader's references for this very spot of this very frame.
pub const CELLS: usize = 2 + BITS as usize;
pub const MODULUS: u32 = 1 << BITS;

/// Where each end puts its pattern, in units of its screen's height: the sender's at the left, the receiver's at the
/// bottom right, so that the receiver's own pattern never covers the sender's mirrored one.
pub const SENDER_SIDE: f64 = 0.38;
pub const RECEIVER_SIDE: f64 = 0.3;
pub const MARGIN: f64 = 0.03;

/// Where the sender's pattern origin (its QR code's top-left corner) goes on its screen, in screen heights from the
/// top-left corner.
pub fn sender_origin() -> (f64, f64) {
    (MARGIN - BOX[0] * SENDER_SIDE, MARGIN - BOX[1] * SENDER_SIDE)
}

/// The same for the receiver's, on a screen `aspect` heights wide.
pub fn receiver_origin(aspect: f64) -> (f64, f64) {
    (
        aspect - MARGIN - BOX[2] * RECEIVER_SIDE,
        1.0 - MARGIN - BOX[3] * RECEIVER_SIDE,
    )
}

/// The payload's first field: which version of this layout, then who drew it.
pub const MAGIC: &str = "LUCH1";

/// The rectangle of cell `i`: left, top, right, bottom.
pub fn cell(i: usize) -> [f64; 4] {
    let (row, col) = (i / PER_ROW, i % PER_ROW);
    let x = col as f64 * CELL_W;
    [x, ROWS[row][0], x + CELL_W, ROWS[row][1]]
}

/// Where a cell's white references are: just above and just below its row, in the white of the box.
pub fn white_refs(i: usize) -> [(f64, f64); 2] {
    let c = cell(i);
    let x = (c[0] + c[2]) / 2.0;
    let row = i / PER_ROW;
    let above = if row == 0 {
        (1.0 + ROWS[0][0]) / 2.0
    } else {
        (ROWS[0][1] + ROWS[1][0]) / 2.0
    };
    let below = if row == 0 {
        (ROWS[0][1] + ROWS[1][0]) / 2.0
    } else {
        (ROWS[1][1] + BOX[3]) / 2.0
    };
    [(x, above), (x, below)]
}

pub fn gray(n: u32) -> u32 {
    let n = n % MODULUS;
    n ^ (n >> 1)
}

pub fn from_gray(g: u32) -> u32 {
    let mut n = g;
    let mut shift = 1;
    while shift < BITS {
        n ^= n >> shift;
        shift <<= 1;
    }
    n % MODULUS
}

/// Whether cell `i` is dark for counter `n`.
pub fn cell_dark(i: usize, n: u32) -> bool {
    match i {
        0 => false,
        1 => true,
        _ => (gray(n) >> (BITS as usize - 1 - (i - 2))) & 1 == 1,
    }
}

/// Who drew a pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// The computer: its pattern is seen twice, on its own screen and mirrored on the TV.
    Sender,
    /// The TV itself, drawn over the mirrored picture.
    Receiver,
}

impl Role {
    fn tag(self) -> &'static str {
        match self {
            Role::Sender => "S",
            Role::Receiver => "R",
        }
    }
}

/// What a QR code says: `LUCH1;S;key=value;...`. Values never hold `;`.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub role: Role,
    pub fields: Vec<(String, String)>,
}

impl Meta {
    pub fn new(role: Role) -> Meta {
        Meta {
            role,
            fields: Vec::new(),
        }
    }

    pub fn with(mut self, key: &str, value: impl ToString) -> Meta {
        let value = value.to_string().replace([';', '\n'], ",");
        if !value.is_empty() {
            self.fields.push((key.to_string(), value));
        }
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn to_text(&self) -> String {
        let mut s = format!("{MAGIC};{}", self.role.tag());
        for (k, v) in &self.fields {
            s.push(';');
            s.push_str(k);
            s.push('=');
            s.push_str(v);
        }
        s
    }

    pub fn parse(text: &str) -> Option<Meta> {
        let mut parts = text.trim().split(';');
        if parts.next()? != MAGIC {
            return None;
        }
        let role = match parts.next()? {
            "S" => Role::Sender,
            "R" => Role::Receiver,
            _ => return None,
        };
        let fields = parts
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Some(Meta { role, fields })
    }
}

/// A QR code as a square of dark (true) modules, without the quiet zone.
#[derive(Clone, Debug)]
pub struct Qr {
    pub size: usize,
    pub dark: Vec<bool>,
}

impl Qr {
    /// Byte mode, error correction L, the smallest version that holds `text`, mask 0: the code is drawn sharp on a
    /// screen, and the fewer modules, the bigger each is for a camera across the room. The receiver's encoder
    /// (QrCode.java) makes the same choices, so both draw the very same code for the same text.
    pub fn encode(text: &str) -> Qr {
        use qrcode::{bits::Bits, canvas::Canvas, canvas::MaskPattern, EcLevel, Version};
        for v in 1..=40 {
            let version = Version::Normal(v);
            let mut bits = Bits::new(version);
            if bits.push_byte_data(text.as_bytes()).is_err()
                || bits.push_terminator(EcLevel::L).is_err()
            {
                continue;
            }
            let Ok((data, ec)) =
                qrcode::ec::construct_codewords(&bits.into_bytes(), version, EcLevel::L)
            else {
                continue;
            };
            let mut canvas = Canvas::new(version, EcLevel::L);
            canvas.draw_all_functional_patterns();
            canvas.draw_data(&data, &ec);
            canvas.apply_mask(MaskPattern::Checkerboard);
            return Qr {
                size: version.width() as usize,
                dark: canvas
                    .into_colors()
                    .into_iter()
                    .map(|c| c == qrcode::Color::Dark)
                    .collect(),
            };
        }
        panic!("{} bytes do not fit a QR code", text.len())
    }

    pub fn get(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.dark[y * self.size + x]
    }

    /// Rows of 0 and 1, for the page.
    pub fn rows(&self) -> Vec<String> {
        (0..self.size)
            .map(|y| {
                (0..self.size)
                    .map(|x| if self.get(x, y) { '1' } else { '0' })
                    .collect()
            })
            .collect()
    }
}

/// Which cell a point of the pattern is in, if any.
fn cell_at(x: f64, y: f64) -> Option<usize> {
    let row = ROWS.iter().position(|r| (r[0]..r[1]).contains(&y))?;
    let col = (x / CELL_W).floor();
    (col >= 0.0 && (col as usize) < PER_ROW).then(|| row * PER_ROW + col as usize)
}

/// What one point of the pattern looks like for counter `n`: Some(true) dark, Some(false) light, None outside the box.
pub fn dark_at(qr: &Qr, n: u32, x: f64, y: f64) -> Option<bool> {
    if !(BOX[0]..BOX[2]).contains(&x) || !(BOX[1]..BOX[3]).contains(&y) {
        return None;
    }
    if (0.0..1.0).contains(&x) && (0.0..1.0).contains(&y) {
        let s = qr.size as f64;
        return Some(qr.get((x * s) as usize, (y * s) as usize));
    }
    Some(cell_at(x, y).is_some_and(|i| cell_dark(i, n)))
}

/// The kind of a point of the pattern, independent of the counter: what the synthetic camera needs to know once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spot {
    Outside,
    Fixed(bool),
    /// Cell `i` (2..CELLS), dark when its bit of the counter is set.
    Bit(u8),
}

pub fn spot_at(qr: &Qr, x: f64, y: f64) -> Spot {
    match (dark_at(qr, 0, x, y), cell_at(x, y)) {
        (None, _) => Spot::Outside,
        (Some(_), Some(i)) if i >= 2 => Spot::Bit(i as u8),
        (Some(d), _) => Spot::Fixed(d),
    }
}

/// The layout as JSON, for the sender's page: it draws from this and has no numbers of its own.
pub fn layout_json() -> serde_json::Value {
    serde_json::json!({
        "box": BOX, "rows": ROWS, "per_row": PER_ROW, "cell_w": CELL_W, "bits": BITS,
        "side": SENDER_SIDE, "origin": sender_origin(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gray_code_changes_one_cell_per_step_and_round_trips() {
        for n in 0..MODULUS + 5 {
            assert_eq!(from_gray(gray(n)), n % MODULUS);
            let changed = (2..CELLS)
                .filter(|&i| cell_dark(i, n) != cell_dark(i, n + 1))
                .count();
            assert_eq!(changed, 1, "from {n} to {}", n + 1);
        }
    }

    #[test]
    fn meta_round_trips_and_rejects_other_codes() {
        let m = Meta::new(Role::Receiver)
            .with("model", "Living room TV")
            .with("dec", "c2.a;b")
            .with("empty", "");
        let text = m.to_text();
        assert_eq!(text, "LUCH1;R;model=Living room TV;dec=c2.a,b");
        assert_eq!(Meta::parse(&text), Some(m));
        assert_eq!(Meta::parse("https://example.com"), None);
        assert_eq!(Meta::parse("LUCH1;X;a=b"), None);
    }

    /// The receiver's QR code for this text, as QrCode.java draws it (QrCodeTest.java holds the same rows).
    pub const GOLDEN_TEXT: &str = "LUCH1;R;v=1.0;mo=Living room TV;a=14;hz=60.0";
    pub const GOLDEN: [&str; 29] = [
        "11111110010011011011001111111",
        "10000010001110010011001000001",
        "10111010111010000001001011101",
        "10111010011100111000001011101",
        "10111010001001111011001011101",
        "10000010010001011101101000001",
        "11111110101010101010101111111",
        "00000000111011010101000000000",
        "11101111101101000101111000100",
        "01010000001100100110101101110",
        "10100111110001100110101000111",
        "11101000100101011100110011001",
        "10110011100011110100001001001",
        "00001001010110100000101101010",
        "11011011111110000100000111111",
        "11001000111011110111001101000",
        "11000111001101101101111101011",
        "00111101000101101000111101100",
        "10000111011001000100110111011",
        "01110001111101000111100010010",
        "10111110000011110101111111110",
        "00000000111111100001100011010",
        "11111110111110101000101010011",
        "10000010101011001110100011010",
        "10111010101111100110111111010",
        "10111010010100011100000011111",
        "10111010100011001011000011001",
        "10000010101101011100101110010",
        "11111110110001100000110111011",
    ];

    #[test]
    fn the_qr_code_is_the_receivers_bit_for_bit_and_reads_back() {
        let qr = Qr::encode(GOLDEN_TEXT);
        assert_eq!(qr.rows(), GOLDEN);
        // Drawn 6 pixels a module with a quiet zone, a QR reader gets the text back.
        let (m, q) = (6, 4);
        let w = (qr.size + 2 * q) * m;
        let px = (0..w * w)
            .map(|i| {
                let (x, y) = ((i % w) / m, (i / w) / m);
                let dark = x >= q && y >= q && qr.get(x - q, y - q);
                if dark {
                    20
                } else {
                    235
                }
            })
            .collect();
        let found = crate::analyze::find(&crate::video::Frame {
            t: 0.0,
            width: w,
            height: w,
            px,
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].meta.to_text(), GOLDEN_TEXT);
    }

    #[test]
    fn cells_and_references_stay_in_the_box_off_the_qr_code() {
        for i in 0..CELLS {
            let c = cell(i);
            assert!(
                c[0] >= 0.0 && c[2] <= BOX[2] && c[1] > 1.0 && c[3] <= BOX[3],
                "cell {i}"
            );
            for (x, y) in white_refs(i) {
                assert!(
                    y > 1.0 && y < BOX[3] && cell_at(x, y).is_none(),
                    "reference of {i}"
                );
            }
        }
    }

    #[test]
    fn the_receivers_pattern_never_covers_the_senders() {
        // A computer screen from 4:3 to 16:9 is shown on a 16:9 TV full height, with bars at the sides.
        for aspect in [4.0 / 3.0, 3.0 / 2.0, 16.0 / 10.0, 16.0 / 9.0] {
            let bars = (16.0 / 9.0 - aspect) / 2.0;
            let right = bars + sender_origin().0 + BOX[2] * SENDER_SIDE;
            assert!(
                right < receiver_origin(16.0 / 9.0).0 + BOX[0] * RECEIVER_SIDE,
                "{aspect}"
            );
            assert!(sender_origin().1 + BOX[3] * SENDER_SIDE <= 1.0);
        }
    }
}
