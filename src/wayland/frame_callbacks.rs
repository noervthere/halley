use std::cell::RefCell;
use std::time::Duration;

use smithay::desktop::utils::surface_primary_scanout_output;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::SurfaceData;

/// Hidden surfaces are allowed one callback roughly every second so clients
/// can make progress without running their render loops at the output rate.
pub const FALLBACK_THROTTLE: Option<Duration> = Some(Duration::from_millis(995));

#[derive(Default)]
struct SurfaceFrameSequence {
    last_sent_at: RefCell<Option<(Output, u32)>>,
}

/// Returns the primary output only once per output refresh sequence.
///
/// `require_visible` is false for compositor-owned live previews: their
/// client surface is captured offscreen and therefore is intentionally absent
/// from the output's final render-element states.
pub fn callback_output(
    surface: &WlSurface,
    states: &SurfaceData,
    output: &Output,
    sequence: u32,
    require_visible: bool,
) -> Option<Output> {
    if require_visible && surface_primary_scanout_output(surface, states).as_ref() != Some(output) {
        return None;
    }

    let throttling = states.data_map.get_or_insert(SurfaceFrameSequence::default);
    let mut last_sent_at = throttling.last_sent_at.borrow_mut();
    if last_sent_at
        .as_ref()
        .is_some_and(|(last_output, last_sequence)| {
            last_output == output && *last_sequence == sequence
        })
    {
        return None;
    }
    *last_sent_at = Some((output.clone(), sequence));
    Some(output.clone())
}

#[cfg(test)]
pub fn already_sent_in_sequence(last: Option<(&str, u32)>, output: &str, sequence: u32) -> bool {
    last.is_some_and(|(last_output, last_sequence)| {
        last_output == output && last_sequence == sequence
    })
}

#[cfg(test)]
mod tests {
    use super::already_sent_in_sequence;

    #[test]
    fn callback_sequence_deduplicates_only_the_same_output_cycle() {
        assert!(already_sent_in_sequence(Some(("DP-1", 7)), "DP-1", 7));
        assert!(!already_sent_in_sequence(Some(("DP-1", 7)), "DP-1", 8));
        assert!(!already_sent_in_sequence(Some(("DP-2", 7)), "DP-1", 7));
        assert!(!already_sent_in_sequence(None, "DP-1", 7));
    }
}
