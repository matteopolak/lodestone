//! Mount-owned browser animation opportunities and independent client servicing.

use crate::platform::Instant;
use std::time::Duration;

const UNLIMITED_FALLBACK_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);

fn timeout_milliseconds(now: Instant, deadline: Instant) -> i32 {
    deadline.saturating_duration_since(now).as_nanos().div_ceil(1_000_000)
        .clamp(1, i32::MAX as u128) as i32
}

fn advance_fallback(deadline: Instant, now: Instant) -> Instant {
    let next = deadline + UNLIMITED_FALLBACK_INTERVAL;
    if next <= now { now + UNLIMITED_FALLBACK_INTERVAL } else { next }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct BrowserWake {
    pub animation: bool,
    pub service: bool,
    pub control: bool,
    pub closed: bool,
}

impl BrowserWake {
    fn ready(self) -> bool {
        self.animation || self.service || self.control || self.closed
    }

    fn animation_arrived(&mut self) {
        if !self.closed { self.animation = true; }
    }

    fn service_arrived(&mut self) {
        if !self.closed { self.service = true; }
    }

    fn control_arrived(&mut self) {
        if !self.closed { self.control = true; }
    }

    fn close(&mut self) {
        *self = Self { closed: true, ..Self::default() };
    }
}

#[cfg(all(target_arch = "wasm32", feature = "runtime-presentation"))]
mod host {
    use super::*;
    use js_sys::{Function, Reflect};
    use std::{cell::RefCell, rc::Rc, task::{Poll, Waker}};
    use wasm_bindgen::{JsCast, JsValue, closure::Closure};

    enum AnimationClock {
        Window(web_sys::Window),
        Worker(web_sys::DedicatedWorkerGlobalScope),
    }

    impl AnimationClock {
        fn request(&self, callback: &Function) -> Result<i32, JsValue> {
            match self {
                Self::Window(window) => window.request_animation_frame(callback),
                Self::Worker(worker) => worker.request_animation_frame(callback),
            }
        }

        fn cancel(&self, id: i32) -> Result<(), JsValue> {
            match self {
                Self::Window(window) => window.cancel_animation_frame(id),
                Self::Worker(worker) => worker.cancel_animation_frame(id),
            }
        }
    }

    struct State {
        global: JsValue,
        timeout: Function,
        clear_timeout: Function,
        animation_clock: Option<AnimationClock>,
        animation_callback: Option<Closure<dyn FnMut(f64)>>,
        timer_callback: Option<Closure<dyn FnMut()>>,
        animation_id: Option<i32>,
        timer_id: Option<i32>,
        timer_deadline: Option<Instant>,
        fallback_deadline: Instant,
        wake: BrowserWake,
        waiter: Option<Waker>,
        animation_count: u64,
        timer_count: u64,
        control_count: u64,
        max_timer_lateness: Duration,
        report_at: Instant,
    }

    impl State {
        fn cancel_animation(&mut self) -> Result<(), JsValue> {
            if let (Some(id), Some(clock)) = (self.animation_id, &self.animation_clock) {
                clock.cancel(id)?;
            }
            self.animation_id = None;
            Ok(())
        }

        fn cancel_timer(&mut self) -> Result<(), JsValue> {
            if let Some(id) = self.timer_id {
                self.clear_timeout.call1(&self.global, &JsValue::from_f64(f64::from(id)))?;
            }
            self.timer_id = None;
            self.timer_deadline = None;
            Ok(())
        }

        fn arm_animation(&mut self, visible: bool) -> Result<(), JsValue> {
            if self.wake.closed || !visible {
                self.cancel_animation()?;
                self.wake.animation = false;
                return Ok(());
            }
            if self.animation_id.is_some() { return Ok(()); }
            let Some(clock) = self.animation_clock.as_ref() else { return Ok(()) };
            let callback = self.animation_callback.as_ref().expect("host owns animation callback");
            match clock.request(callback.as_ref().unchecked_ref()) {
                Ok(id) => self.animation_id = Some(id),
                Err(error) => {
                    self.animation_clock = None;
                    self.wake.animation = false;
                    self.fallback_deadline = Instant::now();
                    crate::net::browser_diagnostic(format_args!("browser pacing mode=deadline fallback reason=animation registration failed: {error:?}"));
                }
            }
            Ok(())
        }

        fn cancel_all(&mut self) {
            if let Err(error) = self.cancel_animation() {
                crate::net::browser_diagnostic(format_args!("browser pacing animation cancellation failed: {error:?}"));
            }
            if let Err(error) = self.cancel_timer() {
                crate::net::browser_diagnostic(format_args!("browser pacing timer cancellation failed: {error:?}"));
            }
        }
    }

    impl Drop for State {
        fn drop(&mut self) {
            self.wake.close();
            self.cancel_all();
        }
    }

    #[derive(Clone)]
    pub(crate) struct BrowserFrameHost(Rc<RefCell<State>>);

    impl std::fmt::Debug for BrowserFrameHost {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let state = self.0.borrow();
            f.debug_struct("BrowserFrameHost")
                .field("animation", &state.animation_clock.is_some())
                .field("closed", &state.wake.closed).finish()
        }
    }

    fn host_function(global: &JsValue, name: &str) -> Result<Function, JsValue> {
        Reflect::get(global, &JsValue::from_str(name))?.dyn_into::<Function>()
            .map_err(|_| JsValue::from_str(name))
    }

    impl BrowserFrameHost {
        pub(crate) fn new() -> Result<Self, JsValue> {
            let global: JsValue = js_sys::global().into();
            let animation_clock = if host_function(&global, "requestAnimationFrame").is_ok()
                && host_function(&global, "cancelAnimationFrame").is_ok()
            {
                if let Some(window) = web_sys::window() {
                    Some(AnimationClock::Window(window))
                } else {
                    global.clone().dyn_into::<web_sys::DedicatedWorkerGlobalScope>()
                        .ok().map(AnimationClock::Worker)
                }
            } else { None };
            let now = Instant::now();
            let state = Rc::new(RefCell::new(State {
                timeout: host_function(&global, "setTimeout")?,
                clear_timeout: host_function(&global, "clearTimeout")?,
                global,
                animation_clock,
                animation_callback: None,
                timer_callback: None,
                animation_id: None,
                timer_id: None,
                timer_deadline: None,
                fallback_deadline: now,
                wake: BrowserWake { control: true, ..BrowserWake::default() },
                waiter: None,
                animation_count: 0,
                timer_count: 0,
                control_count: 0,
                max_timer_lateness: Duration::ZERO,
                report_at: now + Duration::from_secs(1),
            }));
            let weak = Rc::downgrade(&state);
            let animation_callback = Closure::wrap(Box::new(move |_timestamp: f64| {
                let Some(state) = weak.upgrade() else { return };
                let waiter = {
                    let mut state = state.borrow_mut();
                    state.animation_id = None;
                    if state.wake.closed { return; }
                    state.animation_count = state.animation_count.saturating_add(1);
                    state.wake.animation_arrived();
                    state.waiter.take()
                };
                if let Some(waiter) = waiter { waiter.wake(); }
            }) as Box<dyn FnMut(f64)>);
            let weak = Rc::downgrade(&state);
            let timer_callback = Closure::wrap(Box::new(move || {
                let Some(state) = weak.upgrade() else { return };
                let waiter = {
                    let mut state = state.borrow_mut();
                    state.timer_id = None;
                    let deadline = state.timer_deadline.take();
                    if state.wake.closed { return; }
                    if let Some(deadline) = deadline {
                        state.max_timer_lateness = state.max_timer_lateness.max(
                            Instant::now().saturating_duration_since(deadline));
                    }
                    state.timer_count = state.timer_count.saturating_add(1);
                    state.wake.service_arrived();
                    state.waiter.take()
                };
                if let Some(waiter) = waiter { waiter.wake(); }
            }) as Box<dyn FnMut()>);
            {
                let mut state = state.borrow_mut();
                state.animation_callback = Some(animation_callback);
                state.timer_callback = Some(timer_callback);
                crate::net::browser_diagnostic(format_args!("browser pacing mode={} reason={}",
                    if state.animation_clock.is_some() { "animation" } else { "deadline fallback" },
                    if state.animation_clock.is_some() { "host APIs available" } else { "animation/cancellation APIs unavailable" }));
            }
            Ok(Self(state))
        }

        pub(crate) fn prepare(
            &self,
            visible: bool,
            service_deadline: Instant,
            presentation_deadline: Option<Instant>,
        ) -> Result<(), JsValue> {
            let mut state = self.0.borrow_mut();
            if state.wake.closed { return Ok(()); }
            state.arm_animation(visible)?;
            let deadline = if visible && state.animation_clock.is_none() {
                service_deadline.min(presentation_deadline.unwrap_or(state.fallback_deadline))
            } else { service_deadline };
            if state.timer_id.is_some() && state.timer_deadline == Some(deadline) {
                return Ok(());
            }
            state.cancel_timer()?;
            let now = Instant::now();
            let callback = state.timer_callback.as_ref().expect("host owns timer callback");
            let id = state.timeout.call2(&state.global, callback.as_ref(),
                &JsValue::from_f64(f64::from(timeout_milliseconds(now, deadline))))?;
            let id = id.as_f64().filter(|id| id.is_finite())
                .ok_or_else(|| JsValue::from_str("setTimeout returned a non-numeric handle"))?;
            state.timer_id = Some(id as i32);
            state.timer_deadline = Some(deadline);
            if now >= state.report_at {
                crate::net::browser_diagnostic(format_args!(
                    "browser pacing callbacks animation/timer/control={}/{}/{} timer_late_max_ms={:.3}",
                    state.animation_count, state.timer_count, state.control_count,
                    state.max_timer_lateness.as_secs_f64() * 1000.0));
                state.animation_count = 0;
                state.timer_count = 0;
                state.control_count = 0;
                state.max_timer_lateness = Duration::ZERO;
                state.report_at = now + Duration::from_secs(1);
            }
            Ok(())
        }

        pub(crate) fn opportunity(
            &self,
            wake: BrowserWake,
            now: Instant,
            visible: bool,
            presentation_deadline: Option<Instant>,
        ) -> bool {
            let mut state = self.0.borrow_mut();
            if !visible || state.wake.closed { return false; }
            if state.animation_clock.is_some() {
                wake.animation && presentation_deadline.is_none_or(|deadline| now >= deadline)
            } else {
                let deadline = presentation_deadline.unwrap_or(state.fallback_deadline);
                if now < deadline { return false; }
                if presentation_deadline.is_none() {
                    state.fallback_deadline = advance_fallback(deadline, now);
                }
                true
            }
        }

        pub(crate) async fn next_wake(&self) -> BrowserWake {
            std::future::poll_fn(|context| {
                let mut state = self.0.borrow_mut();
                if state.wake.ready() {
                    let wake = state.wake;
                    state.wake = BrowserWake { closed: wake.closed, ..BrowserWake::default() };
                    Poll::Ready(wake)
                } else {
                    if state.waiter.as_ref().is_none_or(|waker| !waker.will_wake(context.waker())) {
                        state.waiter = Some(context.waker().clone());
                    }
                    Poll::Pending
                }
            }).await
        }

        pub(crate) fn notify_control(&self) {
            let waiter = {
                let mut state = self.0.borrow_mut();
                if state.wake.closed { return; }
                state.control_count = state.control_count.saturating_add(1);
                state.wake.control_arrived();
                state.waiter.take()
            };
            if let Some(waiter) = waiter { waiter.wake(); }
        }

        pub(crate) fn shutdown(&self) {
            let waiter = {
                let mut state = self.0.borrow_mut();
                state.wake.close();
                state.cancel_all();
                state.waiter.take()
            };
            if let Some(waiter) = waiter { waiter.wake(); }
        }
    }
}

#[cfg(all(target_arch = "wasm32", feature = "runtime-presentation"))]
pub(super) use host::BrowserFrameHost;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_timer_wait_rounds_up_and_stalled_fallback_rebases_once() {
        let start = Instant::now();
        assert_eq!(timeout_milliseconds(start + Duration::from_micros(2300),
            start + Duration::from_nanos(16_666_666)), 15);
        assert_eq!(timeout_milliseconds(start + Duration::from_secs(2), start), 1);
        assert_eq!(advance_fallback(start, start + Duration::from_secs(2)),
            start + Duration::from_secs(2) + UNLIMITED_FALLBACK_INTERVAL);
    }

    #[test]
    fn service_does_not_consume_animation_and_shutdown_rejects_queued_sources() {
        let mut wake = BrowserWake::default();
        wake.service_arrived();
        assert!(wake.ready() && wake.service && !wake.animation);
        wake.animation_arrived();
        assert!(wake.service && wake.animation);
        wake.close();
        wake.animation_arrived();
        wake.service_arrived();
        wake.control_arrived();
        assert!(wake.closed && wake.ready());
        assert!(!wake.animation && !wake.service && !wake.control);
    }
}
