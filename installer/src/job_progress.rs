use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use serde_json::Value;
use windows::Win32::Foundation::ERROR_IO_PENDING;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_NO_PROXY, WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
    WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE, WINHTTP_CALLBACK_STATUS_READ_COMPLETE,
    WINHTTP_CALLBACK_STATUS_REQUEST_ERROR, WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE,
    WINHTTP_DISABLE_AUTHENTICATION, WINHTTP_DISABLE_COOKIES, WINHTTP_DISABLE_REDIRECTS,
    WINHTTP_FLAG_ASYNC, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_OPTION_CONTEXT_VALUE, WINHTTP_OPTION_DISABLE_FEATURE,
    WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET, WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE,
    WINHTTP_WEB_SOCKET_STATUS, WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE,
    WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen,
    WinHttpOpenRequest, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption,
    WinHttpSetStatusCallback, WinHttpSetTimeouts, WinHttpWebSocketCompleteUpgrade,
    WinHttpWebSocketReceive,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::setup::validate_comfy_url;

const MAX_MESSAGE_BYTES: usize = 64 * 1024;
const CHUNK_BYTES: usize = 8 * 1024;
const RECONNECT_DELAY: Duration = Duration::from_millis(750);
const CANCEL_CHECK_INTERVAL: Duration = Duration::from_millis(100);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub value: f64,
    pub max: f64,
}

#[derive(Debug, PartialEq)]
struct NodeProgress {
    prompt_id: String,
    node: String,
    progress: Progress,
}

pub struct Observer {
    latest: Arc<Mutex<Option<NodeProgress>>>,
    _cancel: Sender<()>,
}

impl Observer {
    pub fn start(url: &str, ctx: egui::Context) -> Option<Self> {
        let url = url::Url::parse(&validate_comfy_url(url).ok()?).ok()?;
        let latest = Arc::new(Mutex::new(None));
        let progress = Arc::clone(&latest);
        let (cancel, stopped) = mpsc::channel();
        thread::Builder::new()
            .name("watch-comfyui-progress".into())
            .spawn(move || {
                while watching(&stopped) {
                    observe(&url, &stopped, |update| {
                        let changed = progress
                            .lock()
                            .is_ok_and(|mut current| apply_message(&mut current, update));
                        if changed {
                            ctx.request_repaint();
                        }
                    });
                    if progress
                        .lock()
                        .is_ok_and(|mut current| current.take().is_some())
                    {
                        ctx.request_repaint();
                    }
                    if stopped.recv_timeout(RECONNECT_DELAY) != Err(RecvTimeoutError::Timeout) {
                        break;
                    }
                }
            })
            .ok()?;
        Some(Self {
            latest,
            _cancel: cancel,
        })
    }

    pub fn latest_for(&self, prompt_id: &str) -> Option<Progress> {
        self.latest
            .lock()
            .ok()?
            .as_ref()
            .filter(|update| update.prompt_id == prompt_id)
            .map(|update| update.progress)
    }
}

fn watching(stopped: &Receiver<()>) -> bool {
    matches!(stopped.try_recv(), Err(TryRecvError::Empty))
}

fn apply_message(latest: &mut Option<NodeProgress>, bytes: &[u8]) -> bool {
    let Ok(message) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let data = &message["data"];
    let Some(prompt_id) = data["prompt_id"].as_str().filter(|id| !id.is_empty()) else {
        return false;
    };
    match message["type"].as_str() {
        Some("progress") => {
            let (Some(node), Some(value), Some(max)) = (
                data["node"].as_str(),
                data["value"].as_f64(),
                data["max"].as_f64(),
            ) else {
                return false;
            };
            if max <= 0.0 || value < 0.0 || value > max || node.is_empty() {
                return false;
            }
            let update = NodeProgress {
                prompt_id: prompt_id.into(),
                node: node.into(),
                progress: Progress { value, max },
            };
            if latest.as_ref() == Some(&update) {
                return false;
            }
            *latest = Some(update);
            true
        }
        Some("progress_state") => {
            let finished = latest.as_ref().is_some_and(|update| {
                update.prompt_id == prompt_id
                    && matches!(
                        data["nodes"][&update.node]["state"].as_str(),
                        Some("finished" | "error")
                    )
            });
            if finished {
                *latest = None;
            }
            finished
        }
        _ => false,
    }
}

struct Handle(NonNull<c_void>);

impl Handle {
    fn new(value: *mut c_void) -> Option<Self> {
        NonNull::new(value).map(Self)
    }

    fn raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // Closing an asynchronous handle cancels its pending operation.
        let _ = unsafe { WinHttpCloseHandle(self.raw()) };
    }
}

#[derive(Debug, PartialEq)]
enum Completion {
    RequestSent,
    HeadersAvailable,
    Read(WINHTTP_WEB_SOCKET_STATUS),
    Error,
}

struct CallbackContext {
    completions: Sender<Completion>,
    buffer: UnsafeCell<[u8; CHUNK_BYTES]>,
}

// WinHTTP owns the one pending read buffer until its completion callback. The
// worker reads it only after that callback posts Read and before its next read.
unsafe impl Sync for CallbackContext {}

unsafe extern "system" fn status_callback(
    _: *mut c_void,
    context: usize,
    status: u32,
    info: *mut c_void,
    info_len: u32,
) {
    if context == 0 {
        return;
    }
    let context = context as *const CallbackContext;
    if status == WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
        // This is the final callback, after WinHTTP has finished with its buffer.
        drop(unsafe { Arc::from_raw(context) });
        return;
    }
    let completion = match status {
        WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => Completion::RequestSent,
        WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => Completion::HeadersAvailable,
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE
            if !info.is_null()
                && info_len as usize == std::mem::size_of::<WINHTTP_WEB_SOCKET_STATUS>() =>
        {
            // WinHTTP owns the status structure only for the duration of this call.
            Completion::Read(unsafe { info.cast::<WINHTTP_WEB_SOCKET_STATUS>().read_unaligned() })
        }
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => Completion::Error,
        _ => return,
    };
    // The context reference is retained until HANDLE_CLOSING on this handle.
    let _ = unsafe { &(*context).completions }.send(completion);
}

fn wait_for(
    completions: &Receiver<Completion>,
    stopped: &Receiver<()>,
    timeout: Duration,
) -> Option<Completion> {
    let start = Instant::now();
    while watching(stopped) {
        let remaining = timeout.checked_sub(start.elapsed())?;
        match completions.recv_timeout(remaining.min(CANCEL_CHECK_INTERVAL)) {
            Ok(completion) => return Some(completion),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

fn observe(url: &url::Url, stopped: &Receiver<()>, mut message: impl FnMut(&[u8])) -> Option<()> {
    let host = HSTRING::from(url.host_str()?);
    let port = url.port_or_known_default()?;
    let flags = if url.scheme() == "https" {
        WINHTTP_FLAG_SECURE
    } else {
        WINHTTP_OPEN_REQUEST_FLAGS(0)
    };
    let null = PCWSTR::null();
    let (notify, completions) = mpsc::channel();
    let context = Arc::new(CallbackContext {
        completions: notify,
        buffer: UnsafeCell::new([0; CHUNK_BYTES]),
    });
    // The validated origin is loopback. Use a fixed path, no proxy, redirects,
    // cookies or ambient credentials. Omitting clientId leaves other sockets alone.
    let (session, connection, socket) = unsafe {
        let session = Handle::new(WinHttpOpen(
            w!("Render Bridge"),
            WINHTTP_ACCESS_TYPE_NO_PROXY,
            null,
            null,
            WINHTTP_FLAG_ASYNC,
        ))?;
        WinHttpSetTimeouts(session.raw(), 2_000, 2_000, 2_000, 4_000).ok()?;
        let notifications = WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
            | WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE
            | WINHTTP_CALLBACK_STATUS_READ_COMPLETE
            | WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
            | WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING;
        let previous =
            WinHttpSetStatusCallback(session.raw(), Some(status_callback), notifications, 0);
        if previous.is_some_and(|callback| callback as usize == usize::MAX) {
            return None;
        }
        let connection = Handle::new(WinHttpConnect(session.raw(), &host, port, 0))?;
        let request = Handle::new(WinHttpOpenRequest(
            connection.raw(),
            w!("GET"),
            w!("/ws"),
            null,
            null,
            ptr::null(),
            flags,
        ))?;
        let disabled =
            WINHTTP_DISABLE_REDIRECTS | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION;
        WinHttpSetOption(
            Some(request.raw()),
            WINHTTP_OPTION_DISABLE_FEATURE,
            Some(&disabled.to_ne_bytes()),
        )
        .ok()?;
        WinHttpSetOption(
            Some(request.raw()),
            WINHTTP_OPTION_UPGRADE_TO_WEB_SOCKET,
            None,
        )
        .ok()?;
        if !watching(stopped) {
            return None;
        }
        let request_context = Arc::into_raw(Arc::clone(&context));
        if WinHttpSetOption(
            Some(request.raw()),
            WINHTTP_OPTION_CONTEXT_VALUE,
            Some(&(request_context as usize).to_ne_bytes()),
        )
        .is_err()
        {
            drop(Arc::from_raw(request_context));
            return None;
        }
        WinHttpSendRequest(request.raw(), None, None, 0, 0, request_context as usize).ok()?;
        if wait_for(&completions, stopped, HANDSHAKE_TIMEOUT)? != Completion::RequestSent {
            return None;
        }
        WinHttpReceiveResponse(request.raw(), ptr::null_mut()).ok()?;
        if wait_for(&completions, stopped, HANDSHAKE_TIMEOUT)? != Completion::HeadersAvailable {
            return None;
        }
        let socket_context = Arc::into_raw(Arc::clone(&context));
        let Some(socket) = Handle::new(WinHttpWebSocketCompleteUpgrade(
            request.raw(),
            Some(socket_context as usize),
        )) else {
            drop(Arc::from_raw(socket_context));
            return None;
        };
        (session, connection, socket)
    };
    let mut text = Vec::new();
    let mut oversized = false;
    while watching(stopped) {
        // The context keeps this buffer alive until the handle's final callback.
        let result = unsafe {
            WinHttpWebSocketReceive(
                socket.raw(),
                context.buffer.get().cast(),
                CHUNK_BYTES as u32,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if result != 0 && result != ERROR_IO_PENDING.0 {
            break;
        }
        let Some(Completion::Read(report)) = wait_for(&completions, stopped, IDLE_TIMEOUT) else {
            break;
        };
        let kind = report.eBufferType;
        if matches!(
            kind,
            WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE
                | WINHTTP_WEB_SOCKET_UTF8_FRAGMENT_BUFFER_TYPE
        ) {
            // Completion means WinHTTP has stopped writing to the buffer.
            let bytes =
                unsafe { &*context.buffer.get() }.get(..report.dwBytesTransferred as usize)?;
            if oversized || text.len() + bytes.len() > MAX_MESSAGE_BYTES {
                oversized = true;
            } else {
                text.extend_from_slice(bytes);
            }
            if kind == WINHTTP_WEB_SOCKET_UTF8_MESSAGE_BUFFER_TYPE {
                if !oversized {
                    message(&text);
                }
                oversized = false;
                text.clear();
            }
        } else if kind == WINHTTP_WEB_SOCKET_CLOSE_BUFFER_TYPE {
            break;
        }
    }
    // Close child handles before their parents.
    drop(socket);
    drop(connection);
    drop(session);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_only_reported_node_progress_and_clears_when_it_finishes() {
        let mut current = None;
        let frame =
            br#"{"type":"progress","data":{"prompt_id":"job-1","node":"3","value":4,"max":8}}"#;
        assert!(apply_message(&mut current, frame));
        assert_eq!(
            current.as_ref().unwrap().progress,
            Progress {
                value: 4.0,
                max: 8.0
            }
        );
        assert!(!apply_message(&mut current, frame));
        assert!(!apply_message(
            &mut current,
            br#"{"type":"progress","data":{"prompt_id":"job-2","node":"3","value":9,"max":8}}"#
        ));
        assert!(!apply_message(&mut current, br#"{"type":"progress_state","data":{"prompt_id":"job-2","nodes":{"3":{"state":"finished"}}}}"#));
        assert!(apply_message(&mut current, br#"{"type":"progress_state","data":{"prompt_id":"job-1","nodes":{"3":{"state":"finished"}}}}"#));
        assert!(current.is_none());
    }
}
