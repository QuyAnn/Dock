//! Icon composition for the dock's macOS-style app icons, plus their drop shadows.
//! Icons are composed once per exe and cached by the caller; nothing here runs per frame.
//!
//! Every icon becomes a full-bleed squircle: art that already fills a rounded square is
//! clipped; other art sits on a light or dark backing squircle (no glass plate behind it).

/// Apple-like superellipse exponent (|u|^N + |v|^N = 1).
const EXP: f32 = 5.0;
/// Blur sigma for `drop-shadow(0 1px 1.5px ...)`: CSS blur radius is 2 sigma.
const SHADOW_SIGMA: f32 = 0.75;
/// Opacity of `rgba(0,0,0,0.45)`.
const SHADOW_ALPHA: f32 = 0.45;

/// Side of a composed dock icon in pixels. Sources (up to 256 px) are reduced to this once, so
/// the 38-DIP icon stays crisp up to 250% scaling without per-frame cost.
pub const ICON_N: u32 = 96;

/// Light backing (Chrome, Explorer, ...): near-white vertical gradient, like macOS app tiles.
const LIGHT_TOP: [f32; 3] = [252.0, 252.0, 252.0];
const LIGHT_BOTTOM: [f32; 3] = [222.0, 224.0, 228.0];
/// Dark navy backing for light / blue monochrome art (Task Manager, white glyphs).
const DARK_TOP: [f32; 3] = [24.0, 44.0, 84.0];
const DARK_BOTTOM: [f32; 3] = [10.0, 22.0, 48.0];
/// Art size on a backing squircle, as a share of the icon side.
const ART_ON_BACKING: f32 = 0.76;

/// How an icon is turned into a full-bleed squircle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backing {
    /// The artwork already fills a rounded square: clip it to the squircle.
    FullBleed,
    Light,
    Dark,
}

/// Composes any RGBA icon (any size) into an `ICON_N` x `ICON_N` macOS-style full-bleed
/// squircle with a transparent exterior. Full-bleed art is clipped; anything else is fitted on a
/// light or dark backing squircle picked from the art (see `choose_backing`). Straight RGBA.
pub fn squircle_mask(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
        return vec![0; (ICON_N * ICON_N * 4) as usize];
    }
    let (sn, sq) = to_square(w as usize, h as usize, rgba);
    let n = ICON_N as usize;
    let backing = choose_backing(sn, &sq);
    let mut out = vec![[0.0f32; 4]; n * n];
    let s = n as f32;
    match backing {
        Backing::FullBleed => {
            let (rn, src) = reduce(sn, &sq, (s * 1.08).ceil() as usize);
            draw_scaled(&mut out, n, &src, rn, s * 1.08);
        }
        Backing::Light | Backing::Dark => {
            let (top, bottom) = if backing == Backing::Light { (LIGHT_TOP, LIGHT_BOTTOM) } else { (DARK_TOP, DARK_BOTTOM) };
            for y in 0..n {
                let t = (y as f32 + 0.5) / s;
                let c: Vec<f32> = (0..3).map(|i| (top[i] + (bottom[i] - top[i]) * t) / 255.0).collect();
                for x in 0..n {
                    out[y * n + x] = [c[0], c[1], c[2], 1.0];
                }
            }
            // Fit the art's visible bounds, so icons with built-in padding are not tiny.
            let (bn, cropped) = crop_to_content(sn, &sq);
            let size = s * ART_ON_BACKING;
            let (rn, src) = reduce(bn, &cropped, size.ceil() as usize);
            draw_scaled(&mut out, n, &src, rn, size);
        }
    }
    // Soft top sheen (liquid glass catch-light).
    let half = s * 0.5;
    for y in 0..n {
        let t = (y as f32 + 0.5) / half;
        if t >= 1.0 {
            break;
        }
        let a = 0.10 * (1.0 - t);
        for x in 0..n {
            composite(&mut out[y * n + x], [a, a, a, a]);
        }
    }
    clip_squircle(&out, n)
}

/// Picks the backing: full-bleed when the art covers the squircle corners; otherwise a dark
/// navy tile for very light art or cool (blue) monochrome art, else a light tile.
pub fn choose_backing(n: usize, rgba: &[u8]) -> Backing {
    let at = |x: usize, y: usize| rgba[(y * n + x) * 4 + 3];
    let lo = (n as f32 * 0.12) as usize;
    let hi = n - 1 - lo;
    let mid = n / 2;
    let edge = (n / 32).max(1);
    let corners = [at(lo, lo), at(hi, lo), at(lo, hi), at(hi, hi)].iter().all(|&a| a > 200);
    let edges = [at(mid, edge), at(mid, n - 1 - edge), at(edge, mid), at(n - 1 - edge, mid)].iter().all(|&a| a > 200);
    let mut opaque = 0usize;
    let (mut lum, mut blue) = (0.0f32, 0usize);
    for p in rgba.chunks_exact(4) {
        if p[3] > 200 {
            opaque += 1;
            let (r, g, b) = (p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
            lum += 0.2126 * r + 0.7152 * g + 0.0722 * b;
            if b - (r + g) / 2.0 > 0.15 {
                blue += 1;
            }
        }
    }
    let coverage = opaque as f32 / (n * n) as f32;
    if coverage > 0.78 && corners && edges {
        return Backing::FullBleed;
    }
    if opaque == 0 {
        return Backing::Light;
    }
    let mean = lum / opaque as f32;
    let blue_share = blue as f32 / opaque as f32;
    if mean > 0.8 || blue_share > 0.6 {
        Backing::Dark
    } else {
        Backing::Light
    }
}

/// Pads a w x h image to a centred square.
fn to_square(w: usize, h: usize, rgba: &[u8]) -> (usize, Vec<u8>) {
    if w == h {
        return (w, rgba.to_vec());
    }
    let n = w.max(h);
    let (ox, oy) = ((n - w) / 2, (n - h) / 2);
    let mut out = vec![0u8; n * n * 4];
    for y in 0..h {
        let src = &rgba[y * w * 4..(y + 1) * w * 4];
        let d = ((y + oy) * n + ox) * 4;
        out[d..d + w * 4].copy_from_slice(src);
    }
    (n, out)
}

/// Square crop around the visible (alpha > 8) bounds, centred.
fn crop_to_content(n: usize, rgba: &[u8]) -> (usize, Vec<u8>) {
    let (mut x0, mut y0, mut x1, mut y1) = (n, n, 0usize, 0usize);
    for y in 0..n {
        for x in 0..n {
            if rgba[(y * n + x) * 4 + 3] > 8 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 || y1 < y0 {
        return (n, rgba.to_vec());
    }
    let side = (x1 - x0 + 1).max(y1 - y0 + 1);
    let cx = (x0 + x1 + 1) as isize / 2;
    let cy = (y0 + y1 + 1) as isize / 2;
    let sx = cx - side as isize / 2;
    let sy = cy - side as isize / 2;
    let mut out = vec![0u8; side * side * 4];
    for y in 0..side {
        for x in 0..side {
            let (ix, iy) = (sx + x as isize, sy + y as isize);
            if ix >= 0 && iy >= 0 && (ix as usize) < n && (iy as usize) < n {
                let s = (iy as usize * n + ix as usize) * 4;
                let d = (y * side + x) * 4;
                out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
            }
        }
    }
    (side, out)
}

/// Halves (2x2 premultiplied average) while the image is at least twice `target`, so the final
/// bilinear step never skips source pixels (no aliasing from 256 px icons).
fn reduce(n: usize, rgba: &[u8], target: usize) -> (usize, Vec<u8>) {
    let (mut n, mut cur) = (n, rgba.to_vec());
    while n >= 2 * target.max(1) && n >= 2 {
        let h = n / 2;
        let mut next = vec![0u8; h * h * 4];
        for y in 0..h {
            for x in 0..h {
                let mut acc = [0.0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = ((2 * y + dy) * n + 2 * x + dx) * 4;
                    let a = cur[i + 3] as f32;
                    acc[0] += cur[i] as f32 * a;
                    acc[1] += cur[i + 1] as f32 * a;
                    acc[2] += cur[i + 2] as f32 * a;
                    acc[3] += a;
                }
                let d = (y * h + x) * 4;
                if acc[3] > 0.0 {
                    next[d] = (acc[0] / acc[3]).round() as u8;
                    next[d + 1] = (acc[1] / acc[3]).round() as u8;
                    next[d + 2] = (acc[2] / acc[3]).round() as u8;
                }
                next[d + 3] = (acc[3] / 4.0).round() as u8;
            }
        }
        n = h;
        cur = next;
    }
    (n, cur)
}

/// Clips a premultiplied canvas to the squircle and returns straight RGBA.
fn clip_squircle(out: &[[f32; 4]], n: usize) -> Vec<u8> {
    let mut dst = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let d = signed_dist(x, y, n);
            let clip = (d + 0.5).clamp(0.0, 1.0);
            let mut p = out[y * n + x];
            for c in p.iter_mut() {
                *c *= clip;
            }
            let a = p[3];
            let (r, g, b) = if a > 0.0 { (p[0] / a, p[1] / a, p[2] / a) } else { (0.0, 0.0, 0.0) };
            dst.extend_from_slice(&[to_u8(r), to_u8(g), to_u8(b), to_u8(a)]);
        }
    }
    dst
}

/// Start button art: white four-pane Windows logo on a system-blue gradient, full-bleed.
pub fn start_icon_rgba() -> Vec<u8> {
    let n = ICON_N as usize;
    let s = n as f32;
    let mut out = vec![[0.0f32; 4]; n * n];
    let (top, bottom) = ([64.0f32, 160.0, 255.0], [0.0f32, 96.0, 214.0]);
    let logo = s * 0.46;
    let gap = s * 0.035;
    let pane = (logo - gap) / 2.0;
    let x0 = (s - logo) / 2.0;
    for y in 0..n {
        let t = (y as f32 + 0.5) / s;
        for x in 0..n {
            let mut c = [0.0f32; 3];
            for i in 0..3 {
                c[i] = (top[i] + (bottom[i] - top[i]) * t) / 255.0;
            }
            // Anti-aliased coverage of the 4 panes (box filter on the pane edges).
            let cov = |p: f32, start: f32| -> f32 {
                let a = (p + 1.0 - start).clamp(0.0, 1.0);
                let b = (p + 1.0 - (start + pane)).clamp(0.0, 1.0);
                (a - b).clamp(0.0, 1.0)
            };
            let (px, py) = (x as f32, y as f32);
            let wx = cov(px, x0) + cov(px, x0 + pane + gap);
            let wy = cov(py, x0) + cov(py, x0 + pane + gap);
            let w = (wx * wy).clamp(0.0, 1.0);
            for i in 0..3 {
                c[i] = c[i] * (1.0 - w) + w;
            }
            out[y * n + x] = [c[0], c[1], c[2], 1.0];
        }
    }
    let half = s * 0.5;
    for y in 0..n {
        let t = (y as f32 + 0.5) / half;
        if t >= 1.0 {
            break;
        }
        let a = 0.10 * (1.0 - t);
        for x in 0..n {
            composite(&mut out[y * n + x], [a, a, a, a]);
        }
    }
    clip_squircle(&out, n)
}

/// Soft macOS-style icon shadow: offset `dy` px down, Gaussian `sigma`, `opacity`.
pub fn soft_shadow(w: u32, h: u32, alpha: &[u8], sigma: f32, opacity: f32, dy: usize) -> (u32, u32, Vec<u8>) {
    shadow_impl(w, h, alpha, sigma, opacity, dy)
}

/// Drop shadow of an alpha mask: black, offset 1 px down, blurred, at 45% opacity.
/// Returns (w, h, rgba) with `pad` extra pixels on each side so the blur is not clipped;
/// the padded size is `w + 2 * pad` where `pad` is `shadow_pad(w)`.
pub fn drop_shadow(w: u32, h: u32, alpha: &[u8]) -> (u32, u32, Vec<u8>) {
    shadow_impl(w, h, alpha, SHADOW_SIGMA, SHADOW_ALPHA, 1)
}

fn shadow_impl(w: u32, h: u32, alpha: &[u8], sigma: f32, opacity: f32, dy: usize) -> (u32, u32, Vec<u8>) {
    let (wu, hu) = (w as usize, h as usize);
    let pad = shadow_pad(w) as usize;
    let (pw, ph) = (wu + 2 * pad, hu + 2 * pad);
    // Offset 1 px down: sample the source one row up.
    let src = |x: isize, y: isize| -> f32 {
        if x < 0 || y < 0 || x >= wu as isize || y >= hu as isize {
            0.0
        } else {
            alpha[y as usize * wu + x as usize] as f32 / 255.0
        }
    };
    let mut buf = vec![0.0f32; pw * ph];
    for y in 0..ph {
        for x in 0..pw {
            buf[y * pw + x] = src(x as isize - pad as isize, y as isize - pad as isize - dy as isize);
        }
    }
    let kernel = gaussian_kernel(sigma);
    let r = (kernel.len() / 2) as isize;
    // Separable blur: horizontal then vertical.
    let mut tmp = vec![0.0f32; pw * ph];
    for y in 0..ph {
        for x in 0..pw {
            let mut acc = 0.0;
            for (i, k) in kernel.iter().enumerate() {
                let sx = x as isize + i as isize - r;
                if sx >= 0 && sx < pw as isize {
                    acc += k * buf[y * pw + sx as usize];
                }
            }
            tmp[y * pw + x] = acc;
        }
    }
    let mut out = Vec::with_capacity(pw * ph * 4);
    for y in 0..ph {
        for x in 0..pw {
            let mut acc = 0.0;
            for (i, k) in kernel.iter().enumerate() {
                let sy = y as isize + i as isize - r;
                if sy >= 0 && sy < ph as isize {
                    acc += k * tmp[sy as usize * pw + x];
                }
            }
            let a = (acc * opacity * 255.0).round().clamp(0.0, 255.0) as u8;
            out.extend_from_slice(&[0, 0, 0, a]);
        }
    }
    (pw as u32, ph as u32, out)
}

/// Padding in source pixels that `drop_shadow` adds on each side for an icon of `w` pixels.
pub fn shadow_pad(w: u32) -> u32 {
    (w / 10).max(2)
}

/// Anti-aliased alpha of a rounded rect filling the whole w x h box (CSS border-radius).
pub fn rounded_rect_alpha(w: u32, h: u32, radius: f32) -> Vec<u8> {
    let (wf, hf) = (w as f32, h as f32);
    let (hx, hy) = (wf / 2.0, hf / 2.0);
    let r = radius.min(hx).min(hy);
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let px = x as f32 + 0.5 - hx;
            let py = y as f32 + 0.5 - hy;
            // Signed distance to a rounded box (negative inside).
            let qx = px.abs() - (hx - r);
            let qy = py.abs() - (hy - r);
            let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
            let inside = qx.max(qy).min(0.0);
            let d = outside + inside - r;
            out.push((((0.5 - d).clamp(0.0, 1.0)) * 255.0).round() as u8);
        }
    }
    out
}

/// Signed distance in px from the pixel centre to the superellipse edge (positive inside).
fn signed_dist(x: usize, y: usize, n: usize) -> f32 {
    let r = n as f32 / 2.0;
    let u = (x as f32 + 0.5 - r) / r;
    let v = (y as f32 + 0.5 - r) / r;
    let f = (u.abs().powf(EXP) + v.abs().powf(EXP)).powf(1.0 / EXP);
    (1.0 - f) * r
}

/// Draws `src` (n x n straight RGBA) centred in the n x n canvas at `size` px, bilinear, source-over.
fn draw_scaled(out: &mut [[f32; 4]], n: usize, src: &[u8], sn: usize, size: f32) {
    let off = (n as f32 - size) / 2.0;
    let scale = sn as f32 / size;
    for y in 0..n {
        for x in 0..n {
            let sx = (x as f32 + 0.5 - off) * scale;
            let sy = (y as f32 + 0.5 - off) * scale;
            if sx < 0.0 || sy < 0.0 || sx >= sn as f32 || sy >= sn as f32 {
                continue;
            }
            let p = sample(src, sn, sx, sy);
            composite(&mut out[y * n + x], p);
        }
    }
}

/// Bilinear sample, returned premultiplied in 0..1.
fn sample(src: &[u8], n: usize, x: f32, y: f32) -> [f32; 4] {
    let fx = x - 0.5;
    let fy = y - 0.5;
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let mut acc = [0.0f32; 4];
    for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
        for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
            let ix = (x0 as isize + dx).clamp(0, n as isize - 1) as usize;
            let iy = (y0 as isize + dy).clamp(0, n as isize - 1) as usize;
            let i = (iy * n + ix) * 4;
            let a = src[i + 3] as f32 / 255.0;
            let wgt = wx * wy;
            acc[0] += wgt * a * src[i] as f32 / 255.0;
            acc[1] += wgt * a * src[i + 1] as f32 / 255.0;
            acc[2] += wgt * a * src[i + 2] as f32 / 255.0;
            acc[3] += wgt * a;
        }
    }
    acc
}

/// Source-over on premultiplied colour.
fn composite(dst: &mut [f32; 4], src: [f32; 4]) {
    let inv = 1.0 - src[3];
    for c in 0..4 {
        dst[c] = src[c] + dst[c] * inv;
    }
}

fn to_u8(v: f32) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Normalised 1D Gaussian kernel with radius 2 sigma.
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let r = (2.0 * sigma).ceil() as isize;
    let mut k: Vec<f32> = (-r..=r)
        .map(|i| (-(i as f32).powi(2) / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = k.iter().sum();
    for v in &mut k {
        *v /= sum;
    }
    k
}
