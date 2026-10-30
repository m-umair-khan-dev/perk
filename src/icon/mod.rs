//! Tray icons for each visual state, rendered once at startup.

mod render;

pub use render::{IconKind, ico, render};

use crate::core::Effective;

/// Tray icon pixel size. Windows scales tray icons down from 32 cleanly; Linux
/// indicators are commonly drawn at 22-48 px.
#[cfg(windows)]
pub const TRAY_SIZE: u32 = 32;
#[cfg(not(windows))]
pub const TRAY_SIZE: u32 = 64;

pub fn kind_for(effective: &Effective) -> IconKind {
    match effective {
        Effective::Active(_) => IconKind::Active,
        Effective::Paused(_, _) => IconKind::Paused,
        Effective::Waiting(_) => IconKind::Waiting,
        Effective::Disabled => IconKind::Off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_expected_buffer_size_and_is_not_blank() {
        for kind in [
            IconKind::Active,
            IconKind::Paused,
            IconKind::Waiting,
            IconKind::Off,
        ] {
            let px = render(kind, 32);
            assert_eq!(px.len(), 32 * 32 * 4);
            let opaque = px.chunks(4).filter(|p| p[3] > 200).count();
            assert!(opaque > 50, "{kind:?} icon looks empty");
            // Corners stay transparent.
            assert_eq!(px[3], 0, "{kind:?} top-left corner");
        }
    }

    #[test]
    fn states_are_visually_distinct() {
        let a = render(IconKind::Active, 32);
        let p = render(IconKind::Paused, 32);
        let w = render(IconKind::Waiting, 32);
        let o = render(IconKind::Off, 32);
        for (i, x) in [&a, &p, &w, &o].iter().enumerate() {
            for y in [&a, &p, &w, &o].iter().skip(i + 1) {
                assert_ne!(x, y);
            }
        }
    }

    #[test]
    fn ico_has_valid_header_and_directory() {
        let sizes = [16, 32, 256];
        let data = ico(IconKind::Active, &sizes);
        assert_eq!(&data[..4], &[0, 0, 1, 0]);
        assert_eq!(u16::from_le_bytes([data[4], data[5]]), 3);
        // Last entry: offset + length == file size.
        let e = 6 + 16 * 2;
        let len = u32::from_le_bytes(data[e + 8..e + 12].try_into().unwrap()) as usize;
        let off = u32::from_le_bytes(data[e + 12..e + 16].try_into().unwrap()) as usize;
        assert_eq!(off + len, data.len());
        assert_eq!(data[e], 0, "256 px is encoded as 0");
    }
}
