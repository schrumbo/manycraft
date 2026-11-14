use std::{
    sync::{
        Arc, LazyLock, OnceLock,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::Thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use gpui::FrameRequestSource;
use gpui_util::ResultExt;
use windows::Win32::{
    Foundation::HWND,
    Graphics::Dwm::{DWM_TIMING_INFO, DwmFlush, DwmGetCompositionTimingInfo},
    System::Performance::QueryPerformanceFrequency,
};

static QPC_TICKS_PER_SECOND: LazyLock<u64> = LazyLock::new(|| {
    let mut frequency = 0;
    // On systems that run Windows XP or later, the function will always succeed and
    // will thus never return zero.
    unsafe { QueryPerformanceFrequency(&mut frequency).unwrap() };
    frequency as u64
});

const VSYNC_INTERVAL_THRESHOLD: Duration = Duration::from_millis(1);
const DEFAULT_VSYNC_INTERVAL: Duration = Duration::from_micros(16_666); // ~60Hz

const FRAME_DEMAND_LINGER: u32 = 4;

/// How long the vsync thread sleeps while no window has frame demand.
pub(crate) const VSYNC_IDLE_TIMEOUT: Duration = Duration::from_secs(1);

/// Wakes the vsync thread, which parks while no window has frame demand.
#[derive(Default)]
pub(crate) struct VSyncWaker(parking_lot::Mutex<Option<Thread>>);

impl VSyncWaker {
    /// Makes the calling thread the one [`Self::wake`] unparks.
    pub(crate) fn register_current(&self) {
        *self.0.lock() = Some(std::thread::current());
    }

    pub(crate) fn wake(&self) {
        if let Some(thread) = &*self.0.lock() {
            thread.unpark();
        }
    }
}

/// Whether the vsync thread should invalidate a window. GPUI wakes it when the
/// window has frame demand; an idle window gets no `WM_PAINT` at all.
pub(crate) struct FrameDemand {
    frames: AtomicU32,
    held: AtomicBool,
    waker: OnceLock<Arc<VSyncWaker>>,
}

impl FrameDemand {
    pub(crate) fn new() -> Self {
        Self {
            frames: AtomicU32::new(FRAME_DEMAND_LINGER),
            held: AtomicBool::new(false),
            waker: OnceLock::new(),
        }
    }

    pub(crate) fn attach(&self, waker: Arc<VSyncWaker>) {
        self.waker.get_or_init(|| waker).wake();
    }

    /// Invalidates the window on the next few vsyncs.
    pub(crate) fn request(&self) {
        self.frames.store(FRAME_DEMAND_LINGER, Ordering::Release);
        if let Some(waker) = self.waker.get() {
            waker.wake();
        }
    }

    /// Invalidates the window on every vsync while held, e.g. during a touchpad gesture.
    pub(crate) fn hold(&self, held: bool) {
        self.held.store(held, Ordering::Release);
        self.request();
    }

    /// Consumes one vsync of demand; true when the window should be invalidated.
    pub(crate) fn take(&self) -> bool {
        self.held.load(Ordering::Acquire)
            || self
                .frames
                .try_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_sub(1))
                .is_ok()
    }
}

pub(crate) struct VSyncProvider {
    interval: Duration,
    f: Box<dyn Fn() -> bool>,
}

impl VSyncProvider {
    pub(crate) fn new() -> Self {
        let interval = get_dwm_interval()
            .context("Failed to get DWM interval")
            .log_err()
            .unwrap_or(DEFAULT_VSYNC_INTERVAL);
        let f = Box::new(|| unsafe { DwmFlush().is_ok() });
        Self { interval, f }
    }

    pub(crate) fn wait_for_vsync(&self) -> FrameRequestSource {
        let vsync_start = Instant::now();
        let wait_succeeded = (self.f)();
        let elapsed = vsync_start.elapsed();
        // DwmFlush and DCompositionWaitForCompositorClock returns very early
        // instead of waiting until vblank when the monitor goes to sleep or is
        // unplugged (nothing to present due to desktop occlusion). We use 1ms as
        // a threshold for the duration of the wait functions and fallback to
        // Sleep() if it returns before that. This could happen during normal
        // operation for the first call after the vsync thread becomes non-idle,
        // but it shouldn't happen often.
        if !wait_succeeded || elapsed < VSYNC_INTERVAL_THRESHOLD {
            log::trace!("VSyncProvider::wait_for_vsync() took less time than expected");
            std::thread::sleep(self.interval);
            FrameRequestSource::LocalSchedule
        } else {
            FrameRequestSource::NativeCallback
        }
    }
}

fn get_dwm_interval() -> Result<Duration> {
    let mut timing_info = DWM_TIMING_INFO {
        cbSize: std::mem::size_of::<DWM_TIMING_INFO>() as u32,
        ..Default::default()
    };
    unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut timing_info) }?;
    let interval = retrieve_duration(timing_info.qpcRefreshPeriod, *QPC_TICKS_PER_SECOND);
    // Check for interval values that are impossibly low. A 29 microsecond
    // interval was seen (from a qpcRefreshPeriod of 60).
    if interval < VSYNC_INTERVAL_THRESHOLD {
        Ok(retrieve_duration(
            timing_info.rateRefresh.uiDenominator as u64,
            timing_info.rateRefresh.uiNumerator as u64,
        ))
    } else {
        Ok(interval)
    }
}

#[inline]
fn retrieve_duration(counts: u64, ticks_per_second: u64) -> Duration {
    let ticks_per_microsecond = ticks_per_second / 1_000_000;
    Duration::from_micros(counts / ticks_per_microsecond)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_demand_lingers_then_stops() {
        let demand = FrameDemand::new();
        for _ in 0..FRAME_DEMAND_LINGER {
            assert!(demand.take());
        }
        assert!(!demand.take());
        demand.request();
        assert!(demand.take());
    }

    #[test]
    fn held_frame_demand_never_runs_out() {
        let demand = FrameDemand::new();
        demand.hold(true);
        for _ in 0..FRAME_DEMAND_LINGER * 4 {
            assert!(demand.take());
        }
        demand.hold(false);
        for _ in 0..FRAME_DEMAND_LINGER {
            assert!(demand.take());
        }
        assert!(!demand.take());
    }

    #[test]
    fn successful_compositor_wait_reports_native_callback() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                true
            }),
        };

        assert_eq!(
            provider.wait_for_vsync(),
            FrameRequestSource::NativeCallback
        );
    }

    #[test]
    fn failed_compositor_wait_reports_local_schedule_even_after_threshold() {
        let provider = VSyncProvider {
            interval: Duration::ZERO,
            f: Box::new(|| {
                std::thread::sleep(VSYNC_INTERVAL_THRESHOLD * 2);
                false
            }),
        };

        assert_eq!(provider.wait_for_vsync(), FrameRequestSource::LocalSchedule);
    }
}
