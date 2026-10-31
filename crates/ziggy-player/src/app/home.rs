//! Home Assistant controls in the core: which tap does what, the brightness bar, optimistic taps.

use super::*;

impl Core {
    pub(super) fn on_home(&mut self, _update: HomeUpdate, _now_ms: u64, _fx: &mut Vec<Effect>) {}
    pub(super) fn on_home_ui(&mut self, _action: UiAction, _now_ms: u64, _fx: &mut Vec<Effect>) {}
    pub(super) fn home_tick(&mut self, _now_ms: u64) {}
}
