//! A browser WebSocket-backed [`Transport`], built on `web-sys`.
//!
//! This is the wasm counterpart of [`crate::WsTransport`]: it lets a browser
//! build reach a Minecraft server through the same protocol-blind WebSocket→TCP
//! relay. Each outbound write becomes one binary WebSocket frame; inbound binary
//! frames are concatenated into the read stream, so the [`crate::Codec`] above
//! sees a continuous byte stream and the relay stays dumb.
//!
//! # Status
//!
//! This module **compiles for `wasm32-unknown-unknown`** under the `ws-web`
//! feature and mirrors the semantics of the native transport that is proven
//! end-to-end against a live server. Its in-browser runtime path is not
//! exercised by this spike (that needs a full wasm client join); it exists to
//! prove the shape compiles and to pin down the `!Send` constraint that a
//! browser transport forces on [`Transport`](crate::Transport).
//!
//! Note it is `!Send` (it holds a `web-sys` `WebSocket`, an `Rc`, and JS
//! closures), which is exactly why the `Transport` trait drops its `Send` bound
//! on wasm.

use std::cell::RefCell;
use std::io;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use crate::inbox::ByteInbox;

/// State shared between the WebSocket event callbacks and the async read side.
#[derive(Default)]
struct Shared {
    /// Bytes received from binary frames, not yet handed to the reader. This is
    /// the same reassembler the native transport uses, so browser reframing of
    /// split/coalesced frames is covered by [`ByteInbox`]'s tests.
    inbox: ByteInbox,
    /// Waker for a reader parked in `poll_read`.
    read_waker: Option<Waker>,
    /// Set once the socket has closed (clean or after an error).
    closed: bool,
    /// Set if the socket reported an error; surfaced to the reader.
    error: Option<String>,
}

impl Shared {
    /// Wakes any parked reader.
    fn wake(&mut self) {
        if let Some(waker) = self.read_waker.take() {
            waker.wake();
        }
    }
}

/// A [`Transport`](crate::Transport) that rides a browser `WebSocket`.
///
/// Construct with [`WsWebTransport::connect`]; the returned transport is only
/// ready once the socket's `open` event has fired, so writes never race the
/// handshake. It is `!Send` by construction (browser objects are single-threaded).
pub struct WsWebTransport {
    ws: WebSocket,
    shared: Rc<RefCell<Shared>>,
    // The closures must outlive the socket, or the browser drops the handlers.
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
}

impl std::fmt::Debug for WsWebTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WsWebTransport")
            .field("url", &self.ws.url())
            .finish_non_exhaustive()
    }
}

impl WsWebTransport {
    /// Opens a WebSocket to `url` (e.g. `ws://127.0.0.1:25580`) and resolves once
    /// it is open and ready to carry bytes.
    ///
    /// # Errors
    ///
    /// Returns [`crate::NetError::Io`] if the socket cannot be created or errors
    /// before it opens.
    pub async fn connect(url: &str) -> crate::Result<Self> {
        let ws = WebSocket::new(url).map_err(js_to_io)?;
        ws.set_binary_type(BinaryType::Arraybuffer);

        let shared = Rc::new(RefCell::new(Shared::default()));

        let on_message = {
            let shared = shared.clone();
            Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
                if let Ok(buffer) = event.data().dyn_into::<js_sys::ArrayBuffer>() {
                    let array = js_sys::Uint8Array::new(&buffer);
                    let mut bytes = vec![0u8; array.length() as usize];
                    array.copy_to(&mut bytes);
                    let mut state = shared.borrow_mut();
                    state.inbox.push(&bytes);
                    state.wake();
                }
            })
        };
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        let on_close = {
            let shared = shared.clone();
            Closure::<dyn FnMut(CloseEvent)>::new(move |_event: CloseEvent| {
                let mut state = shared.borrow_mut();
                state.closed = true;
                state.wake();
            })
        };
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        // `open` and pre-open `error` both resolve the connect future.
        let (open_tx, open_rx) = oneshot::channel::<Result<(), String>>();
        let open_tx = Rc::new(RefCell::new(Some(open_tx)));

        let on_error = {
            let shared = shared.clone();
            let open_tx = open_tx.clone();
            // The WebSocket `error` event is a bare `Event` per the WHATWG spec —
            // it deliberately carries no detail (a failed connection must not leak
            // *why* to script). Typing the closure as `ErrorEvent` and reading
            // `.message()` returns `undefined` at runtime, which then panics inside
            // wasm-bindgen's string marshalling (`undefined.length`). Real browser
            // testing against a down relay surfaced exactly that. Use a fixed label.
            Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                let message = "websocket error".to_string();
                let mut state = shared.borrow_mut();
                state.error = Some(message.clone());
                state.closed = true;
                state.wake();
                // If the error arrived before `open`, fail the connect future.
                if let Some(tx) = open_tx.borrow_mut().take() {
                    let _ = tx.send(Err(message));
                }
            })
        };
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        let on_open = {
            let open_tx = open_tx.clone();
            Closure::<dyn FnMut(Event)>::new(move |_event: Event| {
                if let Some(tx) = open_tx.borrow_mut().take() {
                    let _ = tx.send(Ok(()));
                }
            })
        };
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        let connect_result: Result<(), crate::NetError> = match open_rx.await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(message)) => Err(io::Error::other(message).into()),
            Err(_canceled) => Err(io::Error::other("websocket closed before opening").into()),
        };

        if let Err(err) = connect_result {
            // Connect failed. The `on_*` closures below are about to be dropped at
            // end of scope, but the socket still holds JS references to them, and a
            // failed connection delivers a trailing `error`/`close` event. If that
            // event fires into an already-freed closure, wasm-bindgen aborts with
            // "closure invoked recursively or after being dropped". Detach every
            // handler and close the socket first so no stray event can land.
            ws.set_onopen(None);
            ws.set_onmessage(None);
            ws.set_onerror(None);
            ws.set_onclose(None);
            let _ = ws.close();
            return Err(err);
        }

        // The open handler has done its job; the error handler stays installed.
        ws.set_onopen(None);
        drop(on_open);

        Ok(Self {
            ws,
            shared,
            _on_message: on_message,
            _on_close: on_close,
            _on_error: on_error,
        })
    }
}

/// Maps a JS value error into the `io::Error` the async traits speak.
fn js_to_io(value: wasm_bindgen::JsValue) -> crate::NetError {
    let message = value
        .as_string()
        .unwrap_or_else(|| "javascript error".to_string());
    io::Error::other(message).into()
}

impl AsyncRead for WsWebTransport {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let mut state = this.shared.borrow_mut();

        if !state.inbox.is_empty() {
            state.inbox.serve(buf);
            return Poll::Ready(Ok(()));
        }
        if let Some(message) = &state.error {
            return Poll::Ready(Err(io::Error::other(message.clone())));
        }
        if state.closed {
            // Clean EOF at a frame boundary.
            return Poll::Ready(Ok(()));
        }
        state.read_waker = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl AsyncWrite for WsWebTransport {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.shared.borrow().closed {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "websocket closed",
            )));
        }
        match this.ws.send_with_u8_array(data) {
            Ok(()) => Poll::Ready(Ok(data.len())),
            Err(value) => Poll::Ready(Err(io::Error::other(
                value
                    .as_string()
                    .unwrap_or_else(|| "send failed".to_string()),
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // The browser buffers and flushes for us.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.ws.close().ok();
        Poll::Ready(Ok(()))
    }
}
