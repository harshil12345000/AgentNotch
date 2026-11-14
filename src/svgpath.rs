//! Minimal SVG path-data parser (the `d` attribute), enough to draw brand logos.
//! Output is absolute segments; arcs are kept as arcs (Direct2D draws them natively).

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Quad(f32, f32, f32, f32),
    Arc { rx: f32, ry: f32, rot: f32, large: bool, sweep: bool, x: f32, y: f32 },
    Close,
}

struct Scan<'a> {
    b: &'a [u8],
    i: usize,
}

impl Scan<'_> {
    fn skip(&mut self) {
        while self.i < self.b.len() && (self.b[self.i].is_ascii_whitespace() || self.b[self.i] == b',') {
            self.i += 1;
        }
    }
    fn peek_cmd(&mut self) -> Option<u8> {
        self.skip();
        self.b.get(self.i).copied().filter(|c| c.is_ascii_alphabetic() && *c != b'e' && *c != b'E')
    }
    /// One number. Handles "-.5", "1e-3" and run-together forms such as "0.5.5" and "1-2".
    fn num(&mut self) -> Option<f32> {
        self.skip();
        let s = self.i;
        let b = self.b;
        let mut i = s;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            let mut j = i + 1;
            if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
                j += 1;
            }
            if j < b.len() && b[j].is_ascii_digit() {
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                i = j;
            }
        }
        let v = std::str::from_utf8(&b[s..i]).ok()?.parse::<f32>().ok()?;
        self.i = i;
        Some(v)
    }
    /// Arc flags are a single 0 or 1 and may be glued to the next number ("013.046").
    fn flag(&mut self) -> Option<bool> {
        self.skip();
        let c = *self.b.get(self.i)?;
        if c == b'0' || c == b'1' {
            self.i += 1;
            Some(c == b'1')
        } else {
            None
        }
    }
}

pub fn parse(d: &str) -> Vec<Seg> {
    let mut sc = Scan { b: d.as_bytes(), i: 0 };
    let mut out = Vec::new();
    let (mut x, mut y, mut sx, mut sy) = (0f32, 0f32, 0f32, 0f32);
    // last control point, for the smooth commands
    let (mut cx, mut cy) = (0f32, 0f32);
    let mut prev = 0u8;
    let mut cmd = 0u8;
    loop {
        match sc.peek_cmd() {
            Some(c) => {
                cmd = c;
                sc.i += 1;
            }
            None => {
                if sc.i >= sc.b.len() || cmd == 0 || cmd == b'Z' || cmd == b'z' {
                    break;
                }
                // implicit repeat; a repeated moveto is a lineto
                cmd = match cmd {
                    b'M' => b'L',
                    b'm' => b'l',
                    c => c,
                };
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let (ox, oy) = if rel { (x, y) } else { (0.0, 0.0) };
        macro_rules! n {
            () => {
                match sc.num() {
                    Some(v) => v,
                    None => break,
                }
            };
        }
        match cmd.to_ascii_uppercase() {
            b'M' => {
                x = n!() + ox;
                y = n!() + oy;
                sx = x;
                sy = y;
                out.push(Seg::Move(x, y));
            }
            b'L' => {
                x = n!() + ox;
                y = n!() + oy;
                out.push(Seg::Line(x, y));
            }
            b'H' => {
                x = n!() + ox;
                out.push(Seg::Line(x, y));
            }
            b'V' => {
                y = n!() + oy;
                out.push(Seg::Line(x, y));
            }
            b'C' => {
                let (x1, y1, x2, y2) = (n!() + ox, n!() + oy, n!() + ox, n!() + oy);
                x = n!() + ox;
                y = n!() + oy;
                cx = x2;
                cy = y2;
                out.push(Seg::Cubic(x1, y1, x2, y2, x, y));
            }
            b'S' => {
                let (x1, y1) = if matches!(prev, b'C' | b'S') { (2.0 * x - cx, 2.0 * y - cy) } else { (x, y) };
                let (x2, y2) = (n!() + ox, n!() + oy);
                x = n!() + ox;
                y = n!() + oy;
                cx = x2;
                cy = y2;
                out.push(Seg::Cubic(x1, y1, x2, y2, x, y));
            }
            b'Q' => {
                let (x1, y1) = (n!() + ox, n!() + oy);
                x = n!() + ox;
                y = n!() + oy;
                cx = x1;
                cy = y1;
                out.push(Seg::Quad(x1, y1, x, y));
            }
            b'T' => {
                let (x1, y1) = if matches!(prev, b'Q' | b'T') { (2.0 * x - cx, 2.0 * y - cy) } else { (x, y) };
                x = n!() + ox;
                y = n!() + oy;
                cx = x1;
                cy = y1;
                out.push(Seg::Quad(x1, y1, x, y));
            }
            b'A' => {
                let (rx, ry, rot) = (n!(), n!(), n!());
                let (Some(large), Some(sweep)) = (sc.flag(), sc.flag()) else { break };
                x = n!() + ox;
                y = n!() + oy;
                out.push(Seg::Arc { rx: rx.abs(), ry: ry.abs(), rot, large, sweep, x, y });
            }
            b'Z' => {
                x = sx;
                y = sy;
                out.push(Seg::Close);
            }
            _ => break,
        }
        prev = cmd.to_ascii_uppercase();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_and_relative() {
        let s = parse("M16 6H8v12h8V6zm4 16H4V2h16v20z");
        assert_eq!(s[0], Seg::Move(16.0, 6.0));
        assert_eq!(s[1], Seg::Line(8.0, 6.0));
        assert_eq!(s[2], Seg::Line(8.0, 18.0));
        assert_eq!(s[5], Seg::Close);
        // "m4 16" is relative to the start of the closed figure (16,6)
        assert_eq!(s[6], Seg::Move(20.0, 22.0));
        assert_eq!(s.len(), 12);
    }

    #[test]
    fn compact_numbers_and_arc_flags() {
        let s = parse("M8.086.457a6.105 6.105 0 013.046-.415");
        assert_eq!(s[0], Seg::Move(8.086, 0.457));
        match s[1] {
            Seg::Arc { rx, large, sweep, x, y, .. } => {
                assert_eq!(rx, 6.105);
                assert!(!large && sweep);
                assert!((x - 11.132).abs() < 1e-3 && (y - 0.042).abs() < 1e-3);
            }
            _ => panic!("expected arc"),
        }
        // implicit lineto after moveto, and smooth cubic reflection
        let s = parse("m1 1 2 2c1 0 1 1 2 1s1 1 2 1");
        assert_eq!(s[1], Seg::Line(3.0, 3.0));
        assert_eq!(s[3], Seg::Cubic(6.0, 4.0, 6.0, 5.0, 7.0, 5.0));
    }

    #[test]
    fn all_logos_parse_to_closed_figures() {
        for def in crate::logos::ALL {
            for p in def.paths {
                let s = parse(p);
                assert!(s.len() > 4, "short path");
                assert!(matches!(s[0], Seg::Move(..)));
                assert!(matches!(s.last(), Some(Seg::Close)), "path did not parse to its end");
            }
        }
    }
}
