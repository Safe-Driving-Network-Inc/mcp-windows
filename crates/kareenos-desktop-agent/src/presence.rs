//! "The person is using the computer" and "the person paused agents".
//!
//! HUMAN_ACTIVE: the person's last input (GetLastInputInfo) is younger than the
//! idle window. The agent's own clicks and keystrokes count as input too, so
//! every action notes the tick it finished at and a reading that falls within a
//! short margin of that tick is ours, not the person's.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const OWN_INPUT_MARGIN_MS: u64 = 400;

#[derive(Debug, Clone, Copy)]
pub struct HumanActive {
    pub idle_ms: u64,
    pub retry_after_ms: u64,
}

pub struct Presence {
    /// 0 = the HUMAN_ACTIVE check is off.
    idle_window_ms: AtomicU64,
    /// GetTickCount at the end of the agent's last own input action.
    last_agent_tick: AtomicU64,
    paused: AtomicBool,
}

impl Presence {
    pub fn new(idle_window_ms: u64) -> Self {
        Presence {
            idle_window_ms: AtomicU64::new(idle_window_ms),
            last_agent_tick: AtomicU64::new(0),
            paused: AtomicBool::new(false),
        }
    }

    pub fn set_idle_window(&self, ms: u64) {
        self.idle_window_ms.store(ms, Ordering::Relaxed);
    }
    pub fn idle_window(&self) -> u64 {
        self.idle_window_ms.load(Ordering::Relaxed)
    }
    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::Relaxed);
    }
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Call right after an action that moved the mouse or typed.
    pub fn note_agent_input(&self) {
        if let Some((now, _)) = last_input_ticks() {
            self.last_agent_tick.store(now as u64, Ordering::Relaxed);
        }
    }

    /// Some(..) when the person used the mouse or keyboard within the window.
    pub fn human_active(&self) -> Option<HumanActive> {
        let window = self.idle_window();
        if window == 0 {
            return None;
        }
        let (now, last_input) = last_input_ticks()?;
        let idle_ms = now.wrapping_sub(last_input) as u64;
        if idle_ms >= window {
            return None;
        }
        // Was that input ours? Our last action ended at last_agent_tick; an input
        // tick at or before it (plus a margin) is the agent's own.
        let agent = self.last_agent_tick.load(Ordering::Relaxed);
        if agent > 0 && (last_input as u64) <= agent + OWN_INPUT_MARGIN_MS {
            return None;
        }
        Some(HumanActive { idle_ms, retry_after_ms: (window - idle_ms).max(1000) })
    }
}

/// (GetTickCount now, tick of the last input event), or None when unavailable.
#[cfg(windows)]
pub fn last_input_ticks() -> Option<(u32, u32)> {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: a plain out-parameter call; the struct is correctly sized.
    let ok = unsafe { GetLastInputInfo(&mut info) }.as_bool();
    if !ok {
        return None;
    }
    let now = unsafe { GetTickCount() };
    Some((now, info.dwTime))
}

#[cfg(not(windows))]
pub fn last_input_ticks() -> Option<(u32, u32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_zero_means_off() {
        let p = Presence::new(0);
        assert!(p.human_active().is_none());
        p.set_idle_window(3000);
        assert_eq!(p.idle_window(), 3000);
        p.set_paused(true);
        assert!(p.is_paused());
    }
}
