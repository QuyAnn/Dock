//! macOS-style icon composition, a CPU port of squircle.ts, plus the `.sq` drop shadow.
//! Icons are composed once per exe and cached by the caller; nothing here runs per frame.

/// Apple-like superellipse exponent (|u|^N + |v|^N = 1).
const EXP: f32 = 5.0;
/// Blur sigma for `drop-shadow(0 1px 1.5px ...)`: CSS blur radius is 2 sigma.
const SHADOW_SIGMA: f32 = 0.75;
/// Opacity of `rgba(0,0,0,0.45)`.
const SHADOW_ALPHA: f32 = 0.45;

/// Composes an icon into the squircle shape: a full-bleed icon fills the mask, anything else sits
/// at 70% on a white (or dark, for white glyphs) plate. Adds the top highlight and the hairline
/// outline. Returns straight RGBA of the same size with alpha 0 outside the squircle.
pub fn squircle_mask(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let n = w as usize;
    if w != h || rgba.len() != n * n * 4 || n == 0 {
        return rgba.to_vec();
    }
    let s = n as f32;

    // Analysis, as in squircle.ts: opaque share, opaque at the four edge midpoints, mean luminance.
    let mut opaque = 0usize;
    let mut lum = 0.0f32;
    for px in rgba.chunks_exact(4) {
        if px[3] > 200 {
            opaque += 1;
            lum += (0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32) / 255.0;
        }
    }
    let at = |x: usize, y: usize| rgba[(y * n + x) * 4 + 3];
    let (mid, lo, hi) = (n / 2, (n / 32).max(1), n - (n / 32).max(1) - 1);
    let edges = [at(mid, lo), at(mid, hi), at(lo, mid), at(hi, mid)].iter().all(|&a| a > 200);
    let coverage = opaque as f32 / (n * n) as f32;
    let mean_lum = if opaque > 0 { lum / opaque as f32 } else { 0.0 };

    // Premultiplied float canvas, composited with source-over.
    let mut out = vec![[0.0f32; 4]; n * n];
    if coverage > 0.78 && edges {
        // Full-bleed artwork: over-scaled by 8% to hide its own corners.
        draw_scaled(&mut out, n, rgba, n, s * 1.08);
    } else {
        // Plate gradient: dark for white glyphs, light otherwise.
        let (top, bottom) = if mean_lum > 0.82 {
            ([0x3a, 0x3a, 0x40], [0x1c, 0x1c, 0x20])
        } else {
            ([0xff, 0xff, 0xff], [0xe3, 0xe3, 0xe8])
        };
        for y in 0..n {
            let t = (y as f32 + 0.5) / s;
            let c = lerp3(top, bottom, t);
            for x in 0..n {
                out[y * n + x] = [c[0], c[1], c[2], 1.0];
            }
        }
        draw_scaled(&mut out, n, rgba, n, s * 0.7);
    }

    // Top highlight: rgba(255,255,255,.22) fading to 0 over the upper half.
    let half = s * 0.5;
    for y in 0..n {
        let t = (y as f32 + 0.5) / half;
        if t >= 1.0 {
            break;
        }
        let a = 0.22 * (1.0 - t);
        for x in 0..n {
            composite(&mut out[y * n + x], [a, a, a, a]);
        }
    }

    // Clip to the squircle, then stroke a 1 px hairline on the edge (not clipped).
    let lw = (s / 64.0).max(1.0);
    let mut dst = Vec::with_capacity(n * n * 4);
    for y in 0..n {
        for x in 0..n {
            let d = signed_dist(x, y, n);
            let clip = (d + 0.5).clamp(0.0, 1.0);
            let mut p = out[y * n + x];
            for c in p.iter_mut() {
                *c *= clip;
            }
            let stroke = (lw / 2.0 - d.abs() + 0.5).clamp(0.0, 1.0) * 0.14;
            composite(&mut p, [0.0, 0.0, 0.0, stroke]);
            let a = p[3];
            let (r, g, b) = if a > 0.0 {
                (p[0] / a, p[1] / a, p[2] / a)
            } else {
                (0.0, 0.0, 0.0)
            };
            dst.extend_from_slice(&[to_u8(r), to_u8(g), to_u8(b), to_u8(a)]);
        }
    }
    dst
}

/// Drop shadow of an alpha mask: black, offset 1 px down, blurred, at 45% opacity.
/// Returns (w, h, rgba) with `pad` extra pixels on each side so the blur is not clipped;
/// the padded size is `w + 2 * pad` where `pad` is `shadow_pad(w)`.
pub fn drop_shadow(w: u32, h: u32, alpha: &[u8]) -> (u32, u32, Vec<u8>) {
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
            buf[y * pw + x] = src(x as isize - pad as isize, y as isize - pad as isize - 1);
        }
    }
    let kernel = gaussian_kernel(SHADOW_SIGMA);
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
            let a = (acc * SHADOW_ALPHA * 255.0).round().clamp(0.0, 255.0) as u8;
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

fn lerp3(a: [u8; 3], b: [u8; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) / 255.0;
    [mix(0), mix(1), mix(2)]
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
