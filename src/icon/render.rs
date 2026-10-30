//! Resolution-independent icon renderer (signed distance fields, anti-aliased).
//!
//! Dependency-free on purpose: `build.rs` includes this file to produce the
//! Windows `.ico`, so the executable icon and the tray icons always match.
//!
//! - Active: filled amber cup with steam.
//! - Paused: amber outline cup with a blue pause badge.
//! - Waiting: amber outline cup (timer starts when the user steps away).
//! - Off:    grey outline cup.
//! - Logo:   the app icon: amber cup on a rounded espresso-brown tile.

#![allow(dead_code)]

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    Active,
    Paused,
    Waiting,
    Off,
    Logo,
}

type Rgb = [f32; 3];

const AMBER: Rgb = [0.961, 0.627, 0.169];
const GREY: Rgb = [0.596, 0.627, 0.659];
const BLUE: Rgb = [0.259, 0.522, 0.957];
const WHITE: Rgb = [1.0, 1.0, 1.0];
const ESPRESSO_TOP: Rgb = [0.333, 0.212, 0.141];
const ESPRESSO_BOTTOM: Rgb = [0.165, 0.102, 0.071];

/// Straight-alpha RGBA8, row-major, top row first.
pub fn render(kind: IconKind, size: u32) -> Vec<u8> {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let u = (x as f32 + 0.5) / s;
            let v = (y as f32 + 0.5) / s;
            let px = shade(kind, u, v, s);
            let a = px[3];
            let un = |c: f32| {
                if a > 0.0 {
                    (c / a).clamp(0.0, 1.0)
                } else {
                    0.0
                }
            };
            out.extend_from_slice(&[
                to_u8(un(px[0])),
                to_u8(un(px[1])),
                to_u8(un(px[2])),
                to_u8(a),
            ]);
        }
    }
    out
}

/// Premultiplied RGBA for one pixel centre at (u, v) in [0, 1]^2.
fn shade(kind: IconKind, u: f32, v: f32, size: f32) -> [f32; 4] {
    if kind == IconKind::Logo {
        return shade_logo(u, v, size);
    }
    let mut c = [0.0f32; 4];
    let cup_color = if kind == IconKind::Off { GREY } else { AMBER };

    // Cup body, handle and saucer.
    let body = round_box(u - 0.40, v - 0.62, 0.26, 0.21, 0.08);
    let body = if kind == IconKind::Active {
        body
    } else {
        body.max(-(body + 0.075)) // outline only
    };
    let handle = ((u - 0.67).hypot(v - 0.60) - 0.11).abs() - 0.04;
    let handle = handle.max(0.64 - u);
    let saucer = round_box(u - 0.42, v - 0.915, 0.37, 0.04, 0.04);
    over(
        &mut c,
        cup_color,
        coverage(body.min(handle).min(saucer), size),
    );

    match kind {
        IconKind::Active => {
            for (i, x0) in [0.27f32, 0.40, 0.53].into_iter().enumerate() {
                let phase = i as f32 * 0.9;
                let d = steam(u, v, x0, phase);
                over(&mut c, AMBER, coverage(d, size) * 0.9);
            }
        }
        IconKind::Paused => {
            let (bx, by, r) = (0.75, 0.27, 0.23);
            let ring = (u - bx).hypot(v - by) - (r + 0.05);
            erase(&mut c, coverage(ring, size));
            over(&mut c, BLUE, coverage((u - bx).hypot(v - by) - r, size));
            let bars = round_box(u - (bx - 0.055), v - by, 0.028, 0.10, 0.02).min(round_box(
                u - (bx + 0.055),
                v - by,
                0.028,
                0.10,
                0.02,
            ));
            over(&mut c, WHITE, coverage(bars, size));
        }
        IconKind::Waiting | IconKind::Off | IconKind::Logo => {}
    }
    c
}

/// Rounded tile with a vertical espresso gradient, the active cup centred on it.
fn shade_logo(u: f32, v: f32, size: f32) -> [f32; 4] {
    const SCALE: f32 = 0.70;
    let mut c = [0.0f32; 4];
    let t = v.clamp(0.0, 1.0);
    let bg = [0, 1, 2].map(|i| ESPRESSO_TOP[i] + (ESPRESSO_BOTTOM[i] - ESPRESSO_TOP[i]) * t);
    over(
        &mut c,
        bg,
        coverage(round_box(u - 0.5, v - 0.5, 0.46, 0.46, 0.20), size),
    );
    // Map into cup space (the cup art is slightly left-heavy, hence the offsets).
    let cu = (u - 0.5) / SCALE + 0.56;
    let cv = (v - 0.5) / SCALE + 0.52;
    let cup = shade(IconKind::Active, cu, cv, size * SCALE);
    for i in 0..3 {
        c[i] = cup[i] + c[i] * (1.0 - cup[3]);
    }
    c[3] = cup[3] + c[3] * (1.0 - cup[3]);
    c
}

/// Wavy vertical stroke with rounded ends.
fn steam(u: f32, v: f32, x0: f32, phase: f32) -> f32 {
    let (top, bottom, half_width) = (0.08f32, 0.33f32, 0.026f32);
    let t = ((v - top) / (bottom - top)).clamp(0.0, 1.0);
    let centre = x0 + 0.035 * (t * std::f32::consts::TAU + phase).sin();
    let dx = u - centre;
    let dv = (top - v).max(v - bottom).max(0.0);
    dx.hypot(dv) - half_width
}

/// Signed distance to an axis-aligned rounded box centred at the origin.
fn round_box(px: f32, py: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = px.abs() - (hx - r);
    let qy = py.abs() - (hy - r);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r
}

fn coverage(d: f32, size: f32) -> f32 {
    (0.5 - d * size).clamp(0.0, 1.0)
}

fn over(c: &mut [f32; 4], rgb: Rgb, a: f32) {
    for i in 0..3 {
        c[i] = rgb[i] * a + c[i] * (1.0 - a);
    }
    c[3] = a + c[3] * (1.0 - a);
}

fn erase(c: &mut [f32; 4], amount: f32) {
    for v in c.iter_mut() {
        *v *= 1.0 - amount;
    }
}

fn to_u8(x: f32) -> u8 {
    (x * 255.0 + 0.5) as u8
}

/// Multi-resolution Windows `.ico` (32-bit BMP entries) of the given kind.
pub fn ico(kind: IconKind, sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&s| bmp_entry(&render(kind, s), s))
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&s, img) in sizes.iter().zip(&images) {
        let dim = if s >= 256 { 0 } else { s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

fn bmp_entry(rgba: &[u8], size: u32) -> Vec<u8> {
    let mask_row = size.div_ceil(32) * 4;
    let mut out = Vec::new();
    for v in [40u32, size, size * 2] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    for v in [0u32, size * size * 4 + mask_row * size, 0, 0, 0, 0] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    out.resize(out.len() + (mask_row * size) as usize, 0);
    out
}
