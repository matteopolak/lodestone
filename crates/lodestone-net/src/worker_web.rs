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

use js_sys::{Array, ArrayBuffer, Uint8Array};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Event, MessageEvent, MessagePort};

use crate::inbox::ByteInbox;

#[derive(Debug, Default)]
struct Shared {
    inbox: ByteInbox,
    reader: Option<Waker>,
    closed: bool,
    error: Option<String>,
}

impl Shared {
    fn wake(&mut self) {
        if let Some(waker) = self.reader.take() {
            waker.wake();
        }
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
        state.error = Some(message.into());
        state.closed = true;
        state.wake();
    }
}

/// A wasm-only [`crate::Transport`] using one `MessageChannel` endpoint.
///
/// This carries raw framed protocol bytes only. Worker launch settings use the
/// owning `Worker` control channel, preventing those messages from becoming
/// plausible packet bytes.
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
        let shared = Rc::new(RefCell::new(Shared::default()));
        let on_message = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                let mut state = shared.borrow_mut();
                match event.data().dyn_into::<ArrayBuffer>() {
                    Ok(buffer) => {
                        state.inbox.push(&Uint8Array::new(&buffer).to_vec());
                    }
                    Err(_) => {
                        state.error = Some("worker port received a non-binary message".to_string());
                        state.closed = true;
                    }
                }
                state.wake();
            })
        };
        port.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        let on_message_error = {
            let shared = Rc::clone(&shared);
            Closure::<dyn FnMut(Event)>::new(move |_| {
                let mut state = shared.borrow_mut();
                state.error = Some("worker port message could not be cloned".to_string());
                state.closed = true;
                state.wake();
            })
        };
        port.set_onmessageerror(Some(on_message_error.as_ref().unchecked_ref()));
        port.start();
        Self { port, shared, _on_message: on_message, _on_message_error: on_message_error }
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
}

impl AsyncRead for MessagePortTransport {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut state = this.shared.borrow_mut();
        if !state.inbox.is_empty() {
            state.inbox.serve(buf);
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
    fn poll_write(self: Pin<&mut Self>, _cx: &mut Context<'_>, data: &[u8]) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.shared.borrow().closed {
            return Poll::Ready(Err(io::Error::new(io::ErrorKind::BrokenPipe, "worker port closed")));
        }
        let bytes = Uint8Array::from(data);
        let transfer = Array::new();
        transfer.push(&bytes.buffer());
        match this.port.post_message_with_transferable(&bytes, &transfer) {
            Ok(()) => Poll::Ready(Ok(data.len())),
            Err(error) => Poll::Ready(Err(io::Error::other(
                error.as_string().unwrap_or_else(|| "worker port send failed".to_string()),
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> { Poll::Ready(Ok(())) }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.get_mut().close();
        Poll::Ready(Ok(()))
    }
}
