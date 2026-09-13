//! A continuous byte stream over a browser [`web_sys::MessagePort`].
//!
//! A `MessagePort` delivers messages, whereas the protocol codec consumes a
//! stream. The shared [`crate::inbox::ByteInbox`] removes that boundary, so a
//! split packet or several coalesced packets arrive at the codec unchanged.

use std::cell::RefCell;
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

use crate::inbox::{ByteCreditWindow, ByteInbox, CreditError};

/// Maximum number of protocol bytes either side may have in flight before the
/// peer drains them. A small fixed window keeps the browser's MessagePort
/// queue bounded while `AsyncWrite::poll_write` still makes progress by
/// returning partial writes.
pub const DEFAULT_MESSAGE_PORT_CREDIT_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct Shared {
    inbox: ByteInbox,
    reader: Option<Waker>,
    writer: Option<Waker>,
    send_credit: ByteCreditWindow,
    receive_credit: ByteCreditWindow,
    closed: bool,
    error: Option<String>,
}

impl Shared {
    fn new(capacity: usize) -> Self {
        Self {
            inbox: ByteInbox::with_capacity(capacity),
            reader: None,
            writer: None,
            send_credit: ByteCreditWindow::empty(capacity),
            receive_credit: ByteCreditWindow::full(capacity),
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
        let shared = Rc::new(RefCell::new(Shared::new(capacity)));
        let on_message = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let mut state = shared.borrow_mut();
                if state.closed {
                    return;
                }
                let data = event.data();
                match data.clone().dyn_into::<ArrayBuffer>() {
                    Ok(buffer) => {
                        let bytes = Uint8Array::new(&buffer).to_vec();
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
                            state.wake_read();
                        }
                    }
                    Err(_) => match parse_credit(&data) {
                        Ok(Some(bytes)) => {
                            if let Err(error) = state.send_credit.release(bytes) {
                                state.fail(credit_error_message(error));
                            } else {
                                state.wake_write();
                            }
                        }
                        Ok(None) => {
                            state.fail("worker port received a non-binary message");
                        }
                        Err(message) => state.fail(message),
                    }
                }
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
        let transport = Self { port, shared, _on_message: on_message, _on_message_error: on_message_error };
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

    fn close(&self) {
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
                state.writer = Some(cx.waker().clone());
                return Poll::Pending;
            }
            send_len
        };
        let bytes = Uint8Array::from(&data[..send_len]);
        let transfer = Array::new();
        transfer.push(&bytes.buffer());
        match this.port.post_message_with_transferable(&bytes, &transfer) {
            Ok(()) => Poll::Ready(Ok(send_len)),
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
