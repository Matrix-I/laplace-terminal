//! Application event loop wakeups and custom event types.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use winit::event_loop::EventLoopProxy;

use crate::keymap;
use crate::update;

#[derive(Debug, Clone)]
pub enum AppEvent {
    WakeUp,
    ChildExit(u64),
    Reopen,
    /// A menu bar item (macOS) was chosen.
    Menu(keymap::Action),
    /// The system switched between Light and Dark appearance (macOS).
    SystemAppearance { dark: bool },
    UpdateCheckResult(Result<update::ReleaseInfo, String>),
    UpdateInstallResult(Result<update::Version, String>),
}

/// Wakes the event loop from PTY reader threads, coalescing bursts of output
/// into a single pending `WakeUp`.
#[derive(Clone)]
pub struct Waker {
    pub proxy: EventLoopProxy<AppEvent>,
    pub queued: Arc<AtomicBool>,
}

impl Waker {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        Waker {
            proxy,
            queued: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn wake(&self) {
        if !self.queued.swap(true, Ordering::AcqRel) {
            let _ = self.proxy.send_event(AppEvent::WakeUp);
        }
    }

    /// Re-arm before rendering so output arriving during the draw schedules another frame.
    pub fn rearm(&self) {
        self.queued.store(false, Ordering::Release);
    }

    pub fn is_queued(&self) -> bool {
        self.queued.load(Ordering::Acquire)
    }
}
