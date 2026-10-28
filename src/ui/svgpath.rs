//! SVG path subset (M L H V C A Z, absolute and relative) drawn as Direct2D geometry.
//! The path is parsed first, so malformed input fails before any D2D resource is touched.
use windows::core::Result;
use windows::Win32::Foundation::E_INVALIDARG;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_BEZIER_SEGMENT, D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_FIGURE_END_OPEN,
    D2D_POINT_2F, D2D_SIZE_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ARC_SEGMENT, D2D1_ARC_SIZE_LARGE, D2D1_ARC_SIZE_SMALL, D2D1_SWEEP_DIRECTION_CLOCKWISE,
    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE, ID2D1GeometrySink,
};

use super::dock::RectF;
use super::renderer::Renderer;

// AI logo glyphs, copied from AiTiles.tsx (viewBox 0 0 24 24). Style per glyph:
//   claude: fill #fff, on bg #d97757        -> draw_path(CLAUDE_SPARK, .., 0xFFFFFFFF, None)
//   codex:  stroke #fff 1.7, fill none, bg #0d0d0d -> draw_path(CODEX_ORBITS, .., 0xFFFFFFFF, Some(1.7))
//   cursor: outline stroke #e8e8e8 1.5, inner stroke 1.2, top face fill #e8e8e8 at 0.85 opacity, bg #141414
//           -> CURSOR_CUBE (1.5), CURSOR_CUBE_INNER (1.2), CURSOR_CUBE_TOP (fill 0xE8E8E8D9)
//   grok:   stroke #fff 2, bg #000 -> draw_path(GROK_SLASH, .., 0xFFFFFFFF, Some(2.0))
// Grok uses round caps in the reference; draw_path uses flat caps, so the ends differ slightly.

/// Claude: eight rounded spokes (rect x=11 y=2.5 w=2 h=9.5 rx=1) rotated 0..315 degrees about (12, 12).
pub const CLAUDE_SPARK: &str = "M13 3.5 L13 11 A1 1 0 0 1 11 11 L11 3.5 A1 1 0 0 1 13 3.5 Z M18.718 6.697 L13.414 12 A1 1 0 0 1 12 10.586 L17.303 5.282 A1 1 0 0 1 18.718 6.697 Z M20.5 13 L13 13 A1 1 0 0 1 13 11 L20.5 11 A1 1 0 0 1 20.5 13 Z M17.303 18.718 L12 13.414 A1 1 0 0 1 13.414 12 L18.718 17.303 A1 1 0 0 1 17.303 18.718 Z M11 20.5 L11 13 A1 1 0 0 1 13 13 L13 20.5 A1 1 0 0 1 11 20.5 Z M5.282 17.303 L10.586 12 A1 1 0 0 1 12 13.414 L6.697 18.718 A1 1 0 0 1 5.282 17.303 Z M3.5 11 L11 11 A1 1 0 0 1 11 13 L3.5 13 A1 1 0 0 1 3.5 11 Z M6.697 5.282 L12 10.586 A1 1 0 0 1 10.586 12 L5.282 6.697 A1 1 0 0 1 6.697 5.282 Z";

/// Codex/ChatGPT: three ellipses (rx 8.5, ry 4) rotated 0, 60, 120 degrees about (12, 12).
pub const CODEX_ORBITS: &str = "M3.5 12 A8.5 4 0 0 1 20.5 12 A8.5 4 0 0 1 3.5 12 Z M7.75 4.639 A8.5 4 60 0 1 16.25 19.361 A8.5 4 60 0 1 7.75 4.639 Z M16.25 4.639 A8.5 4 120 0 1 7.75 19.361 A8.5 4 120 0 1 16.25 4.639 Z";

/// Cursor: hexagon outline (stroke 1.5).
pub const CURSOR_CUBE: &str = "M12 2.5 20.5 7.3v9.4L12 21.5 3.5 16.7V7.3Z";
/// Cursor: inner edges (stroke 1.2).
pub const CURSOR_CUBE_INNER: &str = "M12 2.5v9.5l8.5 4.7M12 12 3.5 16.7";
/// Cursor: top face (fill).
pub const CURSOR_CUBE_TOP: &str = "M12 12 20.5 7.3 12 2.5 3.5 7.3Z";

/// Grok: diagonal slash plus two arcs (stroke 2).
pub const GROK_SLASH: &str = "M4.5 19.5 20 4M9.5 5.2A7.6 7.6 0 0 1 18.8 14.5M14.5 18.8A7.6 7.6 0 0 1 5.2 9.5";

type P = (f32, f32);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Move(P),
    Line(P),
    Cubic(P, P, P),
    Arc { to: P, rx: f32, ry: f32, rot: f32, large: bool, sweep: bool },
    Close,
}

fn bad() -> windows::core::Error {
    E_INVALIDARG.into()
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl Lexer<'_> {
    fn skip_sep(&mut self) {
        while matches!(self.s.get(self.i), Some(b' ' | b',' | b'\t' | b'\r' | b'\n')) {
            self.i += 1;
        }
    }

    /// True if the next token (after separators) starts a number.
    fn has_number(&mut self) -> bool {
        self.skip_sep();
        matches!(self.s.get(self.i), Some(b'0'..=b'9' | b'.' | b'-' | b'+'))
    }

    fn num(&mut self) -> Result<f32> {
        self.skip_sep();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
            self.i += 1;
        }
        let mut digits = false;
        let mut dot = false;
        while let Some(&c) = self.s.get(self.i) {
            if c.is_ascii_digit() {
                digits = true;
            } else if c == b'.' && !dot {
                dot = true;
            } else {
                break;
            }
            self.i += 1;
        }
        if !digits {
            return Err(bad());
        }
        if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
            let mut j = self.i + 1;
            if matches!(self.s.get(j), Some(b'+' | b'-')) {
                j += 1;
            }
            if matches!(self.s.get(j), Some(c) if c.is_ascii_digit()) {
                self.i = j;
                while matches!(self.s.get(self.i), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
        }
        // The slice is ASCII by construction, so UTF-8 decoding cannot fail.
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|t| t.parse::<f32>().ok())
            .ok_or_else(bad)
    }

    /// Arc flags are a single '0' or '1', so they may be packed without separators.
    fn flag(&mut self) -> Result<bool> {
        self.skip_sep();
        let v = match self.s.get(self.i) {
            Some(b'0') => false,
            Some(b'1') => true,
            _ => return Err(bad()),
        };
        self.i += 1;
        Ok(v)
    }

    fn pair(&mut self) -> Result<P> {
        Ok((self.num()?, self.num()?))
    }
}

/// Parses the path into absolute ops. Relative commands are resolved against the current point.
fn parse(d: &str) -> Result<Vec<Op>> {
    let mut lx = Lexer { s: d.as_bytes(), i: 0 };
    let mut ops = Vec::new();
    let mut cur: P = (0.0, 0.0);
    let mut start: P = (0.0, 0.0);
    let mut open = false;
    let mut cmd = 0u8;
    loop {
        lx.skip_sep();
        let Some(&c) = lx.s.get(lx.i) else { break };
        if c.is_ascii_alphabetic() {
            cmd = c;
            lx.i += 1;
        } else if cmd == 0 || cmd.eq_ignore_ascii_case(&b'Z') || !lx.has_number() {
            // Numbers need a command; Z takes no arguments.
            return Err(bad());
        }
        if ops.is_empty() && !cmd.eq_ignore_ascii_case(&b'M') {
            return Err(bad());
        }
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        if !open && up != b'M' && up != b'Z' {
            // After Z, drawing resumes from the subpath start (implicit move).
            ops.push(Op::Move(start));
            open = true;
        }
        let base = cur;
        let to_abs = |p: P| if rel { (base.0 + p.0, base.1 + p.1) } else { p };
        match up {
            b'M' => {
                let p = to_abs(lx.pair()?);
                ops.push(Op::Move(p));
                cur = p;
                start = p;
                open = true;
                // Extra coordinate pairs after M are implicit line-tos.
                cmd = if rel { b'l' } else { b'L' };
            }
            b'L' => {
                let p = to_abs(lx.pair()?);
                ops.push(Op::Line(p));
                cur = p;
            }
            b'H' => {
                let x = lx.num()?;
                let p = (if rel { base.0 + x } else { x }, base.1);
                ops.push(Op::Line(p));
                cur = p;
            }
            b'V' => {
                let y = lx.num()?;
                let p = (base.0, if rel { base.1 + y } else { y });
                ops.push(Op::Line(p));
                cur = p;
            }
            b'C' => {
                let a = to_abs(lx.pair()?);
                let b = to_abs(lx.pair()?);
                let p = to_abs(lx.pair()?);
                ops.push(Op::Cubic(a, b, p));
                cur = p;
            }
            b'A' => {
                let rx = lx.num()?.abs();
                let ry = lx.num()?.abs();
                let rot = lx.num()?;
                let large = lx.flag()?;
                let sweep = lx.flag()?;
                let p = to_abs(lx.pair()?);
                ops.push(Op::Arc { to: p, rx, ry, rot, large, sweep });
                cur = p;
            }
            b'Z' => {
                ops.push(Op::Close);
                cur = start;
                open = false;
            }
            _ => return Err(bad()),
        }
    }
    Ok(ops)
}

/// Writes the ops to the sink. `map` converts path units to DIPs; `scale` converts radii.
fn emit(sink: &ID2D1GeometrySink, ops: &[Op], map: impl Fn(P) -> D2D_POINT_2F, scale: f32) {
    let mut open = false;
    // SAFETY: the caller keeps the sink open for the whole call; every figure is ended below.
    unsafe {
        for op in ops {
            match *op {
                Op::Move(p) => {
                    if open {
                        sink.EndFigure(D2D1_FIGURE_END_OPEN);
                    }
                    sink.BeginFigure(map(p), D2D1_FIGURE_BEGIN_FILLED);
                    open = true;
                }
                Op::Line(p) => sink.AddLine(map(p)),
                Op::Cubic(a, b, c) => sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                    point1: map(a),
                    point2: map(b),
                    point3: map(c),
                }),
                Op::Arc { to, rx, ry, rot, large, sweep } => sink.AddArc(&D2D1_ARC_SEGMENT {
                    point: map(to),
                    size: D2D_SIZE_F { width: rx * scale, height: ry * scale },
                    rotationAngle: rot,
                    sweepDirection: if sweep {
                        D2D1_SWEEP_DIRECTION_CLOCKWISE
                    } else {
                        D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                    },
                    arcSize: if large { D2D1_ARC_SIZE_LARGE } else { D2D1_ARC_SIZE_SMALL },
                }),
                Op::Close => {
                    sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                    open = false;
                }
            }
        }
        if open {
            sink.EndFigure(D2D1_FIGURE_END_OPEN);
        }
    }
}

/// Draws an SVG path `d` (viewBox is `view_box` square, origin 0,0) scaled uniformly into `rect`
/// and centered. Fills when `stroke` is None; otherwise strokes with that width in viewBox units.
pub fn draw_path(
    r: &mut Renderer,
    d: &str,
    rect: RectF,
    view_box: f32,
    rgba: u32,
    stroke: Option<f32>,
) -> Result<()> {
    if view_box <= 0.0 {
        return Err(bad());
    }
    let ops = parse(d)?;
    let scale = rect.w.min(rect.h) / view_box;
    let ox = rect.x + (rect.w - view_box * scale) / 2.0;
    let oy = rect.y + (rect.h - view_box * scale) / 2.0;
    let map = move |p: P| D2D_POINT_2F { x: ox + p.0 * scale, y: oy + p.1 * scale };
    r.draw_geometry_with(
        |sink| {
            emit(sink, &ops, map, scale);
            Ok(())
        },
        rgba,
        stroke.map(|w| w * scale),
    )
}

/// `draw_path` with round caps and joins (icon strokes).
pub fn draw_path_round(r: &mut Renderer, d: &str, rect: RectF, view_box: f32, rgba: u32, stroke: Option<f32>) -> Result<()> {
    if view_box <= 0.0 {
        return Err(bad());
    }
    let ops = parse(d)?;
    let scale = rect.w.min(rect.h) / view_box;
    let ox = rect.x + (rect.w - view_box * scale) / 2.0;
    let oy = rect.y + (rect.h - view_box * scale) / 2.0;
    let map = move |p: P| D2D_POINT_2F { x: ox + p.0 * scale, y: oy + p.1 * scale };
    r.draw_geometry_round(
        |sink| {
            emit(sink, &ops, map, scale);
            Ok(())
        },
        rgba,
        stroke.map(|w| w * scale),
    )
}

#[cfg(test)]
pub fn parse_ok(d: &str) -> bool {
    parse(d).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implicit_lines_and_relative_v() {
        let ops = parse("M12 2.5v9.5l8.5 4.7").unwrap();
        assert_eq!(
            ops,
            vec![Op::Move((12.0, 2.5)), Op::Line((12.0, 12.0)), Op::Line((20.5, 16.7))]
        );
    }

    #[test]
    fn packed_arc_flags_and_implicit_move_after_close() {
        let ops = parse("M0 0a1 1 0 01 2 0Z l1 1").unwrap();
        assert_eq!(ops[1], Op::Arc { to: (2.0, 0.0), rx: 1.0, ry: 1.0, rot: 0.0, large: false, sweep: true });
        assert_eq!(ops[2], Op::Close);
        assert_eq!(ops[3], Op::Move((0.0, 0.0)));
        assert_eq!(ops[4], Op::Line((1.0, 1.0)));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse("L1 1").is_err());
        assert!(parse("M1 1 2").is_err());
        assert!(parse("M1 1 Z 3").is_err());
        assert!(parse("M1 1 X").is_err());
    }

    #[test]
    fn glyphs_parse() {
        for d in [CLAUDE_SPARK, CODEX_ORBITS, CURSOR_CUBE, CURSOR_CUBE_INNER, CURSOR_CUBE_TOP, GROK_SLASH] {
            assert!(parse(d).is_ok(), "{d}");
        }
    }
}
