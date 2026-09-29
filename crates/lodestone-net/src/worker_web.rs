//! A continuous byte stream over a browser [`web_sys::MessagePort`].
//!
//! A `MessagePort` delivers messages, whereas the protocol codec consumes a
//! stream. The shared [`crate::inbox::ByteInbox`] removes that boundary, so a
//! split packet or several coalesced packets arrive at the codec unchanged.

use std::cell::{Cell, RefCell};
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use js_sys::{Array, ArrayBuffer, Object, Reflect, Uint8Array};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsValue;
use web_sys::{Event, MessageEvent, MessagePort};

use crate::inbox::{ByteCreditWindow, ByteInbox, ByteTransportDiagnostics, CreditError};

thread_local! {
    static NEXT_ENDPOINT_ID: Cell<u32> = const { Cell::new(1) };
}

fn next_endpoint_id() -> u32 {
    NEXT_ENDPOINT_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1).max(1));
        id
    })
}

/// Maximum number of protocol bytes either side may have in flight before the
/// peer drains them. A small fixed window keeps the browser's MessagePort
/// queue bounded while `AsyncWrite::poll_write` still makes progress by
/// returning partial writes.
pub const DEFAULT_MESSAGE_PORT_CREDIT_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct Shared {
    endpoint_id: u32,
    inbox: ByteInbox,
    reader: Option<Waker>,
    writer: Option<Waker>,
    send_credit: ByteCreditWindow,
    receive_credit: ByteCreditWindow,
    diagnostics: Option<ByteTransportDiagnostics>,
    closed: bool,
    error: Option<String>,
}

struct DiagnosticsHeartbeat {
    id: JsValue,
    _callback: Closure<dyn FnMut()>,
}

impl DiagnosticsHeartbeat {
    fn start(shared: &Rc<RefCell<Shared>>) -> Option<Self> {
        let shared = Rc::downgrade(shared);
        let callback = Closure::<dyn FnMut()>::new(move || {
            if let Some(shared) = shared.upgrade() {
                shared.borrow_mut().report_diagnostics();
            }
        });
        let global = js_sys::global();
        let set_interval = Reflect::get(&global, &JsValue::from_str("setInterval"))
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()?;
        let id = set_interval
            .call2(&global, callback.as_ref(), &JsValue::from_f64(1000.0))
            .ok()?;
        Some(Self { id, _callback: callback })
    }
}

impl Drop for DiagnosticsHeartbeat {
    fn drop(&mut self) {
        let global = js_sys::global();
        if let Ok(clear_interval) = Reflect::get(&global, &JsValue::from_str("clearInterval"))
            && let Ok(clear_interval) = clear_interval.dyn_into::<js_sys::Function>()
        {
            let _ = clear_interval.call1(&global, &self.id);
        }
    }
}

impl Shared {
    fn new(capacity: usize, diagnostics_enabled: bool) -> Self {
        Self {
            endpoint_id: next_endpoint_id(),
            inbox: ByteInbox::with_capacity(capacity),
            reader: None,
            writer: None,
            send_credit: ByteCreditWindow::empty(capacity),
            receive_credit: ByteCreditWindow::full(capacity),
            diagnostics: diagnostics_enabled.then(ByteTransportDiagnostics::default),
            closed: false,
            error: None,
        }
    }

    fn wake_read(&mut self) {
        if let Some(waker) = self.reader.take() {
            waker.wake();
        }
    }

    fn wake_write(&mut self) {
        if let Some(waker) = self.writer.take() {
            waker.wake();
        }
    }

    fn wake(&mut self) {
        self.wake_read();
        self.wake_write();
    }

    fn report_diagnostics(&mut self) {
        let Some(diagnostics) = self.diagnostics.as_mut() else {
            return;
        };
        let now = lodestone_time::Instant::now();
        if !diagnostics.report_due(now) {
            return;
        }
        log::debug!(
            target: "message_port",
            "browser byte transport diagnostics: endpoint={} posted_bytes={} received_bytes={} drained_bytes={} send_credit={} receive_credit={} longest_write_pending_ms={:.3} current_write_pending_ms={:.3}",
            self.endpoint_id,
            diagnostics.posted_bytes(),
            diagnostics.received_bytes(),
            diagnostics.drained_bytes(),
            self.send_credit.available(),
            self.receive_credit.available(),
            diagnostics.longest_write_pending().as_secs_f64() * 1000.0,
            diagnostics.current_write_pending(now).as_secs_f64() * 1000.0,
        );
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.error = Some(message.into());
        self.closed = true;
        self.wake();
    }
}

/// A page-side liveness signal for a [`MessagePortTransport`].
///
/// `MessagePort` has no close event. The owning `Worker` therefore uses this
/// handle to wake a parked read when it reports a post-ready crash.
#[derive(Clone, Debug)]
pub struct MessagePortShutdown(Rc<RefCell<Shared>>);

impl MessagePortShutdown {
    /// Ends pending and future reads with `message`.
    pub fn signal(&self, message: impl Into<String>) {
        let mut state = self.0.borrow_mut();
        state.fail(message);
    }
}

/// A wasm-only [`crate::Transport`] using one `MessageChannel` endpoint.
///
/// This carries raw framed protocol bytes plus private credit envelopes.
/// Worker launch settings use the owning `Worker` control channel, preventing
/// those messages from becoming plausible packet bytes.
pub struct MessagePortTransport {
    port: MessagePort,
    shared: Rc<RefCell<Shared>>,
    diagnostics_heartbeat: Option<DiagnosticsHeartbeat>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_message_error: Closure<dyn FnMut(Event)>,
}

impl std::fmt::Debug for MessagePortTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MessagePortTransport").finish_non_exhaustive()
    }
}

impl MessagePortTransport {
    /// Starts `port` and takes responsibility for closing it on shutdown.
    #[must_use]
    pub fn new(port: MessagePort) -> Self {
        Self::with_capacity(port, DEFAULT_MESSAGE_PORT_CREDIT_BYTES)
    }

    /// Starts `port` with a finite byte-credit window.
    ///
    /// Both endpoints send an initial credit grant. Subsequent grants are sent
    /// only after the local reader drains bytes, so a sender cannot enqueue
    /// more than the receiver's configured capacity in the browser.
    #[must_use]
    pub fn with_capacity(port: MessagePort, capacity: usize) -> Self {
        let diagnostics_enabled = log::log_enabled!(
            target: "message_port",
            log::Level::Debug
        );
        let shared = Rc::new(RefCell::new(Shared::new(capacity, diagnostics_enabled)));
        let on_message = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let mut state = shared.borrow_mut();
                if state.closed {
                    return;
                }
                let data = event.data();
                if let Some(bytes) = binary_message(&data) {
                    let received_len = bytes.len();
                    if state.receive_credit.available() < bytes.len()
                        || state.inbox.remaining_capacity() < bytes.len()
                        || state.inbox.try_push(&bytes).is_err()
                    {
                        state.fail("worker port exceeded receive credit");
                    } else {
                        // The capacity check above makes this infallible;
                        // keeping the operation explicit documents the
                        // two halves of the receive window.
                        let _ = state.receive_credit.consume_exact(bytes.len());
                        if let Some(diagnostics) = state.diagnostics.as_mut() {
                            diagnostics.record_received(received_len);
                        }
                        state.wake_read();
                    }
                } else {
                    match parse_credit(&data) {
                        Ok(Some(bytes)) => {
                            if let Err(error) = state.send_credit.release(bytes) {
                                state.fail(credit_error_message(error));
                            } else {
                                if let Some(diagnostics) = state.diagnostics.as_mut() {
                                    diagnostics.write_resumed(lodestone_time::Instant::now());
                                }
                                state.wake_write();
                            }
                        }
                        Ok(None) => {
                            state.fail("worker port received a non-binary message");
                        }
                        Err(message) => state.fail(message),
                    }
                }
                state.report_diagnostics();
            })
        };
        port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        let on_message_error = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(Event)>::new(move |_| {
                let mut state = shared.borrow_mut();
                state.fail("worker port message could not be cloned");
            })
        };
        port.set_onmessageerror(Some(on_message_error.as_ref().unchecked_ref()));
        port.start();
        let diagnostics_heartbeat = diagnostics_enabled
            .then(|| DiagnosticsHeartbeat::start(&shared))
            .flatten();
        let transport = Self {
            port,
            shared,
            diagnostics_heartbeat,
            _on_message: on_message,
            _on_message_error: on_message_error,
        };
        // A peer may not have installed its handler yet, but MessagePort queues
        // this message until `start`/handler installation, so construction
        // order does not affect the initial grant.
        transport.send_credit(capacity);
        transport
    }

    /// Returns a liveness handle for the owner of the peer Worker.
    #[must_use]
    pub fn shutdown_handle(&self) -> MessagePortShutdown {
        MessagePortShutdown(Rc::clone(&self.shared))
    }

    fn close(&mut self) {
        self.diagnostics_heartbeat.take();
        self.port.set_onmessage(None);
        self.port.set_onmessageerror(None);
        self.port.close();
        let mut state = self.shared.borrow_mut();
        state.closed = true;
        state.wake();
    }

    fn send_credit(&self, bytes: usize) {
        if bytes == 0 {
            return;
        }
        let message = Object::new();
        if let Err(error) = Reflect::set(
            &message,
            &JsValue::from_str("kind"),
            &JsValue::from_str("credit"),
        ) {
            self.shared.borrow_mut().fail(js_error_message(error));
            return;
        }
        if let Err(error) = Reflect::set(
            &message,
            &JsValue::from_str("bytes"),
            &JsValue::from_f64(bytes as f64),
        ) {
            self.shared.borrow_mut().fail(js_error_message(error));
            return;
        }
        let message: JsValue = message.into();
        if let Err(error) = self.port.post_message(&message) {
            self.shared.borrow_mut().fail(js_error_message(error));
        }
    }
}

impl Drop for MessagePortTransport {
    fn drop(&mut self) {
        // `poll_shutdown` is the normal async path, but startup failure and a
        // cancelled client future can drop the transport without polling it.
        // Closing here detaches callbacks and releases the endpoint in both
        // cases; the peer's Worker is terminated by the owning shell transport.
        self.close();
    }
}

/// `postMessage` preserves a transferred typed-array view, so the peer may
/// receive either that view or its backing buffer depending on the browser.
fn binary_message(value: &JsValue) -> Option<Vec<u8>> {
    if let Ok(buffer) = value.clone().dyn_into::<ArrayBuffer>() {
        return Some(Uint8Array::new(&buffer).to_vec());
    }
    value
        .clone()
        .dyn_into::<Uint8Array>()
        .ok()
        .map(|view| view.to_vec())
}

/// Parses the one non-binary message understood on the data port. Any other
/// object is rejected so launch/control envelopes cannot be mistaken for
/// protocol bytes.
fn parse_credit(value: &JsValue) -> Result<Option<usize>, String> {
    if !value.is_object() || value.is_null() {
        return Ok(None);
    }
    let kind = Reflect::get(value, &JsValue::from_str("kind")).map_err(js_error_message)?;
    if kind.is_undefined() {
        return Ok(None);
    }
    if kind.as_string().as_deref() != Some("credit") {
        return Ok(None);
    }
    let bytes = Reflect::get(value, &JsValue::from_str("bytes")).map_err(js_error_message)?;
    let Some(bytes) = bytes.as_f64() else {
        return Err("worker port credit was not a number".to_string());
    };
    if !bytes.is_finite() || bytes < 0.0 || bytes.fract() != 0.0 || bytes > usize::MAX as f64 {
        return Err("worker port credit was not a valid byte count".to_string());
    }
    Ok(Some(bytes as usize))
}

fn credit_error_message(error: CreditError) -> String {
    match error {
        CreditError::Exceeded { available, requested } => {
            format!("worker port payload exceeded credit ({requested} > {available})")
        }
        CreditError::Overflow { available, returned, limit } => {
            format!("worker port credit exceeded capacity ({available} + {returned} > {limit})")
        }
    }
}

fn js_error_message(error: JsValue) -> String {
    error.as_string().unwrap_or_else(|| "worker port operation failed".to_string())
}

impl AsyncRead for MessagePortTransport {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut state = this.shared.borrow_mut();
        if !state.inbox.is_empty() {
            let served = state.inbox.serve(buf);
            // `serve` cannot exceed the bytes previously consumed from the
            // receive window. The only failure would indicate an internal
            // accounting bug, so close rather than widening the window.
            if state.receive_credit.release(served).is_err() {
                state.fail("worker port receive credit accounting failed");
                return Poll::Ready(Err(io::Error::other(
                    "worker port receive credit accounting failed",
                )));
            }
            if let Some(diagnostics) = state.diagnostics.as_mut() {
                diagnostics.record_drained(served);
            }
            state.report_diagnostics();
            drop(state);
            this.send_credit(served);
            return Poll::Ready(Ok(()));
        }
        if let Some(error) = &state.error {
            return Poll::Ready(Err(io::Error::other(error.clone())));
        }
        if state.closed {
            return Poll::Ready(Ok(()));
        }
        state.reader = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl AsyncWrite for MessagePortTransport {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let send_len = {
            let mut state = this.shared.borrow_mut();
            if state.closed {
                return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "worker port closed")));
            }
            if data.is_empty() {
                return Poll::Ready(Ok(0));
            }
            let send_len = state.send_credit.take(data.len());
            if send_len == 0 {
                if let Some(diagnostics) = state.diagnostics.as_mut() {
                    diagnostics.write_pending(lodestone_time::Instant::now());
                }
                state.writer = Some(cx.waker().clone());
                state.report_diagnostics();
                return Poll::Pending;
            }
            send_len
        };
        let bytes = Uint8Array::from(&data[..send_len]);
        let transfer = Array::new();
        transfer.push(&bytes.buffer());
        match this.port.post_message_with_transferable(&bytes, &transfer) {
            Ok(()) => {
                let mut state = this.shared.borrow_mut();
                if let Some(diagnostics) = state.diagnostics.as_mut() {
                    diagnostics.record_posted(send_len);
                    diagnostics.write_resumed(lodestone_time::Instant::now());
                }
                state.report_diagnostics();
                Poll::Ready(Ok(send_len))
            }
            Err(error) => {
                let message = js_error_message(error);
                this.shared.borrow_mut().fail(message.clone());
                Poll::Ready(Err(io::Error::other(message)))
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().close();
        Poll::Ready(Ok(()))
    }
}
