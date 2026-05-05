//! Diff background-color variants (aligned with CC's bright/dim diff modes).
//!
//! Bright (default): saturated single-channel backgrounds (rgb(0,60,0) / rgb(60,0,0)).
//!   Maximum legibility on dark terminals; matches the original CC default.
//!
//! Dim: balanced low-saturation backgrounds (rgb(20,40,20) / rgb(40,20,20)).
//!   Easier on the eyes during long sessions and works better against very dark
//!   terminal backgrounds where high-saturation greens/reds visually "vibrate".

use std::sync::atomic::{AtomicU8, Ordering};

use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum DiffStyle {
    #[default]
    Bright = 0,
    Dim = 1,
}

impl std::str::FromStr for DiffStyle {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bright" | "vivid" | "high" => Ok(DiffStyle::Bright),
            "dim" | "muted" | "low" => Ok(DiffStyle::Dim),
            _ => Err(()),
        }
    }
}

/// Background colors for one diff variant.
#[derive(Debug, Clone, Copy)]
pub struct DiffPalette {
    pub added_bg: Color,
    pub removed_bg: Color,
    pub added_word_bg: Color,
    pub removed_word_bg: Color,
}

const BRIGHT_PALETTE: DiffPalette = DiffPalette {
    added_bg: Color::Rgb(0, 60, 0),
    removed_bg: Color::Rgb(60, 0, 0),
    added_word_bg: Color::Rgb(0, 100, 0),
    removed_word_bg: Color::Rgb(100, 0, 0),
};

const DIM_PALETTE: DiffPalette = DiffPalette {
    added_bg: Color::Rgb(20, 40, 20),
    removed_bg: Color::Rgb(40, 20, 20),
    added_word_bg: Color::Rgb(35, 75, 35),
    removed_word_bg: Color::Rgb(75, 35, 35),
};

static PALETTES: [DiffPalette; 2] = [BRIGHT_PALETTE, DIM_PALETTE];
static STYLE_IDX: AtomicU8 = AtomicU8::new(0);

pub fn init(style: DiffStyle) {
    STYLE_IDX.store(style as u8, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn set(style: DiffStyle) {
    STYLE_IDX.store(style as u8, Ordering::Relaxed);
}

#[allow(dead_code)]
pub fn current() -> DiffStyle {
    match STYLE_IDX.load(Ordering::Relaxed) {
        1 => DiffStyle::Dim,
        _ => DiffStyle::Bright,
    }
}

/// Active palette for the current global mode.
pub fn palette() -> DiffPalette {
    PALETTES[STYLE_IDX.load(Ordering::Relaxed) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_aliases() {
        assert_eq!("bright".parse::<DiffStyle>().unwrap(), DiffStyle::Bright);
        assert_eq!("Vivid".parse::<DiffStyle>().unwrap(), DiffStyle::Bright);
        assert_eq!("DIM".parse::<DiffStyle>().unwrap(), DiffStyle::Dim);
        assert_eq!("muted".parse::<DiffStyle>().unwrap(), DiffStyle::Dim);
        assert!("rainbow".parse::<DiffStyle>().is_err());
    }

    #[test]
    fn palette_dim_is_less_saturated_than_bright() {
        // Saturation proxy: max channel - min channel.
        fn sat(c: Color) -> u16 {
            match c {
                Color::Rgb(r, g, b) => {
                    let max = r.max(g).max(b);
                    let min = r.min(g).min(b);
                    u16::from(max) - u16::from(min)
                }
                _ => 0,
            }
        }
        assert!(sat(BRIGHT_PALETTE.added_bg) > sat(DIM_PALETTE.added_bg));
        assert!(sat(BRIGHT_PALETTE.removed_bg) > sat(DIM_PALETTE.removed_bg));
    }

    #[test]
    fn set_and_read_global() {
        set(DiffStyle::Dim);
        assert_eq!(current(), DiffStyle::Dim);
        set(DiffStyle::Bright);
        assert_eq!(current(), DiffStyle::Bright);
    }
}
