//! Spinner and turn-completion verbs.

use rand::RngExt;

/// The verb used when the model is in thinking/reasoning mode.
pub(crate) const THINKING_VERB: &str = "Thinking";

/// Unicode marker for thinking blocks (∴ = U+2234).
pub(crate) const THINKING_MARKER: &str = "\u{2234}";

/// Unicode marker for turn-completion messages (✻ = U+273B).
pub(crate) const TURN_COMPLETION_MARKER: &str = "\u{273B}";

/// Unicode marker for info/record messages (⏺ = U+23FA).
pub(crate) const INFO_MARKER: &str = "\u{23FA}";

/// Unicode marker for error messages (✗ = U+2717).
pub(crate) const ERROR_MARKER: &str = "\u{2717}";

/// Unicode marker for warning messages (⚠ = U+26A0).
pub(crate) const WARNING_MARKER: &str = "\u{26A0}";

/// Present-tense verbs shown during generation (spinner label).
/// Source: official Claude Code `spinnerVerbs.ts` (187 entries).
pub(crate) const SPINNER_VERBS: &[&str] = &[
    "Accomplishing",
    "Actioning",
    "Actualizing",
    "Architecting",
    "Baking",
    "Beaming",
    "Beboppin'",
    "Befuddling",
    "Billowing",
    "Blanching",
    "Bloviating",
    "Boogieing",
    "Boondoggling",
    "Booping",
    "Bootstrapping",
    "Brewing",
    "Bunning",
    "Burrowing",
    "Calculating",
    "Canoodling",
    "Caramelizing",
    "Cascading",
    "Catapulting",
    "Cerebrating",
    "Channeling",
    "Channelling",
    "Choreographing",
    "Churning",
    "Clauding",
    "Coalescing",
    "Cogitating",
    "Combobulating",
    "Composing",
    "Computing",
    "Concocting",
    "Considering",
    "Contemplating",
    "Cooking",
    "Crafting",
    "Creating",
    "Crunching",
    "Crystallizing",
    "Cultivating",
    "Deciphering",
    "Deliberating",
    "Determining",
    "Dilly-dallying",
    "Discombobulating",
    "Doing",
    "Doodling",
    "Drizzling",
    "Ebbing",
    "Effecting",
    "Elucidating",
    "Embellishing",
    "Enchanting",
    "Envisioning",
    "Evaporating",
    "Fermenting",
    "Fiddle-faddling",
    "Finagling",
    "Flambéing",
    "Flibbertigibbeting",
    "Flowing",
    "Flummoxing",
    "Fluttering",
    "Forging",
    "Forming",
    "Frolicking",
    "Frosting",
    "Gallivanting",
    "Galloping",
    "Garnishing",
    "Generating",
    "Gesticulating",
    "Germinating",
    "Gitifying",
    "Grooving",
    "Gusting",
    "Harmonizing",
    "Hashing",
    "Hatching",
    "Herding",
    "Honking",
    "Hullaballooing",
    "Hyperspacing",
    "Ideating",
    "Imagining",
    "Improvising",
    "Incubating",
    "Inferring",
    "Infusing",
    "Ionizing",
    "Jitterbugging",
    "Julienning",
    "Kneading",
    "Leavening",
    "Levitating",
    "Lollygagging",
    "Manifesting",
    "Marinating",
    "Meandering",
    "Metamorphosing",
    "Misting",
    "Moonwalking",
    "Moseying",
    "Mulling",
    "Mustering",
    "Musing",
    "Nebulizing",
    "Nesting",
    "Newspapering",
    "Noodling",
    "Nucleating",
    "Orbiting",
    "Orchestrating",
    "Osmosing",
    "Perambulating",
    "Percolating",
    "Perusing",
    "Philosophising",
    "Photosynthesizing",
    "Pollinating",
    "Pondering",
    "Pontificating",
    "Pouncing",
    "Precipitating",
    "Prestidigitating",
    "Processing",
    "Proofing",
    "Propagating",
    "Puttering",
    "Puzzling",
    "Quantumizing",
    "Razzle-dazzling",
    "Razzmatazzing",
    "Recombobulating",
    "Reticulating",
    "Roosting",
    "Ruminating",
    "Sautéing",
    "Scampering",
    "Schlepping",
    "Scurrying",
    "Seasoning",
    "Shenaniganing",
    "Shimmying",
    "Simmering",
    "Skedaddling",
    "Sketching",
    "Slithering",
    "Smooshing",
    "Sock-hopping",
    "Spelunking",
    "Spinning",
    "Sprouting",
    "Stewing",
    "Sublimating",
    "Swirling",
    "Swooping",
    "Symbioting",
    "Synthesizing",
    "Tempering",
    "Thinking",
    "Thundering",
    "Tinkering",
    "Tomfoolering",
    "Topsy-turvying",
    "Transfiguring",
    "Transmuting",
    "Twisting",
    "Undulating",
    "Unfurling",
    "Unravelling",
    "Vibing",
    "Waddling",
    "Wandering",
    "Warping",
    "Whatchamacalliting",
    "Whirlpooling",
    "Whirring",
    "Whisking",
    "Wibbling",
    "Working",
    "Wrangling",
    "Zesting",
    "Zigzagging",
];

/// Past-tense verbs shown on turn completion.
/// Source: official Claude Code `turnCompletionVerbs.ts`.
pub(crate) const TURN_COMPLETION_VERBS: &[&str] = &[
    "Baked",
    "Brewed",
    "Churned",
    "Cogitated",
    "Cooked",
    "Crunched",
    "Sautéed",
    "Worked",
];

/// Pick a random spinner verb.
pub(crate) fn random_spinner_verb() -> &'static str {
    let mut rng = rand::rng();
    let idx = rng.random_range(0..SPINNER_VERBS.len());
    SPINNER_VERBS[idx]
}

/// Pick a random turn-completion verb.
pub(crate) fn random_turn_verb() -> &'static str {
    let mut rng = rand::rng();
    let idx = rng.random_range(0..TURN_COMPLETION_VERBS.len());
    TURN_COMPLETION_VERBS[idx]
}

/// Format a duration in milliseconds to a human-readable string.
pub(crate) fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        let s = ms as f64 / 1000.0;
        if ms.is_multiple_of(1000) {
            format!("{}s", ms / 1000)
        } else {
            format!("{s:.1}s")
        }
    } else if ms < 3_600_000 {
        let mins = ms / 60_000;
        let secs = (ms % 60_000) / 1000;
        if secs == 0 {
            format!("{mins}m")
        } else {
            format!("{mins}m {secs}s")
        }
    } else {
        let hours = ms / 3_600_000;
        let mins = (ms % 3_600_000) / 60_000;
        if mins == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {mins}m")
        }
    }
}

/// Spinner frame tick interval in milliseconds.
pub(crate) const SPINNER_TICK_INTERVAL_MS: u64 = 80;
/// Shimmer speed per step: requesting (forward) = 50ms, responding (reverse) = 200ms.
/// Aligned with official CC's glimmerSpeedMs.
pub(crate) const SHIMMER_REQUESTING_MS: u64 = 50;
pub(crate) const SHIMMER_RESPONDING_MS: u64 = 200;
/// Width of the shimmer highlight segment in characters.
const SHIMMER_WIDTH: usize = 3;

/// Split `text` at a character index (not byte index), returning the byte offset.
pub(crate) fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Shimmer direction: requesting sweeps forward (fast), responding sweeps backward (slow).
#[derive(Clone, Copy)]
pub(crate) enum ShimmerDirection {
    Requesting,
    Responding,
}

/// Compute shimmer segments for a label.
/// Returns `(before, shimmer, after)` where shimmer is a 3-character
/// highlight window that sweeps across the text.
/// All spinner verbs are ASCII, so byte indices equal char indices.
///
/// Aligned with official CC `computeShimmerSegments` + `computeGlimmerIndex`:
/// - Requesting: forward sweep (left to right)
/// - Responding: reverse sweep (right to left)
/// - Cycle length = label.len() + 20 (matches official CC's `messageWidth + 20`)
pub(crate) fn compute_shimmer_segments(
    label: &str,
    tick: usize,
    direction: ShimmerDirection,
) -> (&str, &str, &str) {
    let len = label.len();
    if len == 0 {
        return ("", "", "");
    }
    let cycle = len + 20;
    let cycle_pos = tick % cycle;

    // Compute glimmer index (center of highlight window).
    let glimmer = match direction {
        // Forward sweep: glimmerIndex = (cyclePosition % cycleLength) - 10
        ShimmerDirection::Requesting => cycle_pos as isize - 10,
        // Reverse sweep: glimmerIndex = messageWidth + 10 - (cyclePosition % cycleLength)
        ShimmerDirection::Responding => (len as isize + 10) - cycle_pos as isize,
    };

    // Highlight window: glimmer ± SHIMMER_WIDTH/2 (3 visual columns).
    let half = SHIMMER_WIDTH as isize / 2;
    let win_start = (glimmer - half).max(0) as usize;
    let win_end = (glimmer + half + 1).min(len as isize).max(0) as usize;

    let before_end = win_start.min(len);
    let shimmer_end = win_end.min(len);

    let before = &label[..before_end];
    let shimmer = if before_end < shimmer_end {
        &label[before_end..shimmer_end]
    } else {
        ""
    };
    let after = if shimmer_end < label.len() {
        &label[shimmer_end..]
    } else {
        ""
    };

    (before, shimmer, after)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_verbs_not_empty() {
        assert!(!SPINNER_VERBS.is_empty());
        assert_eq!(SPINNER_VERBS.len(), 187);
    }

    #[test]
    fn turn_verbs_not_empty() {
        assert!(!TURN_COMPLETION_VERBS.is_empty());
        assert_eq!(TURN_COMPLETION_VERBS.len(), 8);
    }

    #[test]
    fn random_spinner_verb_is_valid() {
        let v = random_spinner_verb();
        assert!(SPINNER_VERBS.contains(&v));
    }

    #[test]
    fn random_turn_verb_is_valid() {
        let v = random_turn_verb();
        assert!(TURN_COMPLETION_VERBS.contains(&v));
    }

    #[test]
    fn format_duration_ms() {
        assert_eq!(format_duration(500), "500ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(5000), "5s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(125_000), "2m 5s");
    }

    #[test]
    fn format_duration_exact_minutes() {
        assert_eq!(format_duration(120_000), "2m");
    }

    #[test]
    fn format_duration_hours() {
        assert_eq!(format_duration(3_780_000), "1h 3m");
    }

    #[test]
    fn shimmer_forward_sweep() {
        // Requesting (forward): tick=11 → glimmer=1, window 0..3 → "Bak"
        let (b, s, a) = compute_shimmer_segments("Baking", 11, ShimmerDirection::Requesting);
        assert_eq!(b, "");
        assert_eq!(s, "Bak");
        assert_eq!(a, "ing");
    }

    #[test]
    fn shimmer_forward_sweep_right() {
        // Requesting: tick=14 → glimmer=4, window 3..6 → "ing"
        let (b, s, a) = compute_shimmer_segments("Baking", 14, ShimmerDirection::Requesting);
        assert_eq!(b, "Bak");
        assert_eq!(s, "ing");
        assert_eq!(a, "");
    }

    #[test]
    fn shimmer_reverse_sweep() {
        // Responding (reverse): tick=11 → glimmer=5, window 4..6 → "ng"
        let (b, s, a) = compute_shimmer_segments("Baking", 11, ShimmerDirection::Responding);
        assert_eq!(b, "Baki");
        assert_eq!(s, "ng");
        assert_eq!(a, "");
    }

    #[test]
    fn shimmer_window_before_label() {
        // Forward: tick=0 → glimmer=-10, window clamped to 0..0 → no shimmer
        let (b, s, a) = compute_shimmer_segments("Baking", 0, ShimmerDirection::Requesting);
        assert_eq!(b, "");
        assert_eq!(s, "");
        assert_eq!(a, "Baking");
    }

    #[test]
    fn shimmer_window_past_end() {
        // Responding: tick=0 → glimmer=16, window clamped past end → no shimmer
        let (b, s, a) = compute_shimmer_segments("Baking", 0, ShimmerDirection::Responding);
        assert_eq!(b, "Baking");
        assert_eq!(s, "");
        assert_eq!(a, "");
    }

    #[test]
    fn shimmer_empty_label() {
        let (b, s, a) = compute_shimmer_segments("", 0, ShimmerDirection::Requesting);
        assert_eq!(b, "");
        assert_eq!(s, "");
        assert_eq!(a, "");
    }
}
