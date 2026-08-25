//! Minimal C ABI for Android JNI/Swift wrappers. The command/event model above
//! remains the primary API; this only owns a Tokio runtime and never persists
//! or logs the caller-supplied PKCS#8 key.

use std::{
    ffi::{CStr, CString, c_char},
    net::SocketAddr,
    panic::AssertUnwindSafe,
    ptr,
    sync::Mutex,
    time::Duration,
};

use host_protocol::{Ed25519PublicKey, PairingToken};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use serde::Deserialize;
use tokio::sync::broadcast;
use zeroize::Zeroizing;

use crate::{MobileClient, MobileClientConfig, MobileClientError, Notification, ServerRequest};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CConfig {
    address: SocketAddr,
    server_name: String,
    host_identity: Ed25519PublicKey,
    device_name: String,
    #[serde(default)]
    pairing_ticket: Option<PairingToken>,
    max_frame_bytes: u32,
    request_timeout_ms: u64,
}

impl TryFrom<CConfig> for MobileClientConfig {
    type Error = MobileClientError;

    fn try_from(value: CConfig) -> Result<Self, Self::Error> {
        Ok(Self {
            address: value.address,
            server_name: value.server_name,
            host_identity: value.host_identity,
            device_name: value.device_name,
            pairing_ticket: value.pairing_ticket,
            max_frame_bytes: value.max_frame_bytes,
            request_timeout: Duration::from_millis(value.request_timeout_ms),
        })
    }
}

/// Generates an Ed25519 PKCS#8 document and returns it as base64url without
/// padding. Platform code must decode and store the bytes in Keychain or
/// Keystore, then pass the recovered bytes to `mobile_client_connect`.
/// Generates a PKCS#8 device key for caller-managed secure storage.
///
/// # Safety
/// If non-null, `error_out` must be writable for one `char *`; returned
/// strings must be released with `mobile_client_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_generate_device_key(
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    let result = std::panic::catch_unwind(|| {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| "secure random generation failed")?;
        CString::new(URL_SAFE_NO_PAD.encode(key.as_ref()))
            .map_err(|_| "failed to encode device key")
    });
    match result {
        Ok(Ok(value)) => value.into_raw(),
        Ok(Err(error)) => {
            set_error(error_out, error);
            ptr::null_mut()
        }
        Err(_) => {
            set_error(error_out, "mobile client panicked");
            ptr::null_mut()
        }
    }
}

pub struct Handle {
    // A multi-thread Tokio runtime supports concurrent `block_on` calls.
    // Requests must not exclude polling/responding: an outbound Codex
    // request can pause until the mobile answers a server request.
    runtime: tokio::runtime::Runtime,
    client: MobileClient,
    notifications: Mutex<broadcast::Receiver<Notification>>,
    server_requests: Mutex<broadcast::Receiver<ServerRequest>>,
}

fn set_error(out: *mut *mut c_char, error: impl ToString) {
    if !out.is_null() {
        let text =
            CString::new(error.to_string()).unwrap_or_else(|_| CString::new("error").unwrap());
        // SAFETY: caller supplies a valid writable error-output pointer.
        unsafe { *out = text.into_raw() };
    }
}

fn input_string<'a>(input: *const c_char) -> Result<&'a str, String> {
    if input.is_null() {
        return Err("null string input".to_owned());
    }
    // SAFETY: C ABI requires a NUL-terminated string valid for this call.
    unsafe { CStr::from_ptr(input) }
        .to_str()
        .map_err(|_| "input is not UTF-8".to_owned())
}

pub(crate) fn connect_handle(config: CConfig, key: &[u8]) -> Result<*mut Handle, String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "failed to create Tokio runtime")?;
    let client = runtime
        .block_on(MobileClient::connect(
            config
                .try_into()
                .map_err(|error: MobileClientError| error.to_string())?,
            key,
        ))
        .map_err(|error| error.to_string())?;
    let notifications = client.subscribe();
    let server_requests = client.subscribe_server_requests();
    Ok(Box::into_raw(Box::new(Handle {
        runtime,
        client,
        notifications: Mutex::new(notifications),
        server_requests: Mutex::new(server_requests),
    })))
}

pub(crate) fn request_json(
    handle: &Handle,
    method: String,
    params_json: String,
) -> Result<String, String> {
    let result = handle
        .runtime
        .block_on(handle.client.request_raw(method, params_json))
        .map_err(encode_mobile_error)?;
    serde_json::to_string(&result).map_err(|_| "failed to encode response".to_owned())
}

fn encode_mobile_error(error: MobileClientError) -> String {
    match error {
        MobileClientError::Remote { error } => error,
        other => other.to_string(),
    }
}

pub(crate) fn respond_result_json(
    handle: &Handle,
    request_id_json: &str,
    result_json: &str,
) -> Result<(), String> {
    handle
        .runtime
        .block_on(handle.client.respond_raw(
            request_id_json.to_owned(),
            "result",
            result_json.to_owned(),
        ))
        .map_err(|error| error.to_string())
}

pub(crate) fn respond_error_json(
    handle: &Handle,
    request_id_json: &str,
    error_json: &str,
) -> Result<(), String> {
    handle
        .runtime
        .block_on(handle.client.respond_raw(
            request_id_json.to_owned(),
            "error",
            error_json.to_owned(),
        ))
        .map_err(|error| error.to_string())
}

pub(crate) fn next_notification_json(handle: &Handle) -> Result<Option<String>, String> {
    let mut notifications = handle
        .notifications
        .lock()
        .map_err(|_| "notification lock poisoned")?;
    match notifications.try_recv() {
        Ok(notification) => Ok(Some(notification)),
        Err(broadcast::error::TryRecvError::Empty) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn next_server_request_json(handle: &Handle) -> Result<Option<String>, String> {
    let mut requests = handle
        .server_requests
        .lock()
        .map_err(|_| "server-request lock poisoned")?;
    match requests.try_recv() {
        Ok(request) => Ok(Some(request)),
        Err(broadcast::error::TryRecvError::Empty) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn close_handle(handle: *mut Handle) {
    if !handle.is_null() {
        // SAFETY: caller transfers ownership exactly once to close.
        let handle = unsafe { Box::from_raw(handle) };
        handle.client.close();
    }
}

/// Connects and returns an opaque mobile-client handle.
///
/// # Safety
/// `config_json` must be a valid NUL-terminated UTF-8 string and
/// `device_pkcs8` must designate `device_pkcs8_len` readable bytes. If
/// non-null, `error_out` must be writable for one `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_connect(
    config_json: *const c_char,
    device_pkcs8: *const u8,
    device_pkcs8_len: usize,
    error_out: *mut *mut c_char,
) -> *mut Handle {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    let result = std::panic::catch_unwind(|| -> Result<*mut Handle, String> {
        let config: CConfig = serde_json::from_str(input_string(config_json)?)
            .map_err(|_| "invalid config JSON".to_owned())?;
        if device_pkcs8.is_null() || device_pkcs8_len == 0 {
            return Err("missing device PKCS#8 bytes".to_owned());
        }
        // SAFETY: caller promises a readable byte range for this call.
        let key = unsafe { std::slice::from_raw_parts(device_pkcs8, device_pkcs8_len) };
        let key = Zeroizing::new(key.to_vec());
        connect_handle(config, &key)
    });
    match result {
        Ok(Ok(handle)) => handle,
        Ok(Err(error)) => {
            set_error(error_out, error);
            ptr::null_mut()
        }
        Err(_) => {
            set_error(error_out, "mobile client panicked");
            ptr::null_mut()
        }
    }
}

/// Sends one request through a live opaque handle.
///
/// # Safety
/// `handle` must be live and exclusively retained by the caller; all
/// string inputs must be valid NUL-terminated UTF-8. If non-null,
/// `error_out` must be writable for one `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_request(
    handle: *mut Handle,
    method: *const c_char,
    params_json: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    if handle.is_null() {
        set_error(error_out, "null mobile client handle");
        return ptr::null_mut();
    }
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<CString, String> {
        let method = input_string(method)?.to_owned();
        let params = input_string(params_json)?.to_owned();
        // SAFETY: checked non-null and the handle remains owned by caller.
        let handle = unsafe { &*handle };
        CString::new(request_json(handle, method, params)?)
            .map_err(|_| "response contains NUL".to_owned())
    }));
    match result {
        Ok(Ok(value)) => value.into_raw(),
        Ok(Err(error)) => {
            set_error(error_out, error);
            ptr::null_mut()
        }
        Err(_) => {
            set_error(error_out, "mobile client panicked");
            ptr::null_mut()
        }
    }
}

/// Returns one queued notification, if available.
///
/// # Safety
/// `handle` must be live. If non-null, `error_out` must be writable for
/// one `char *`; non-null returned strings use `mobile_client_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_next_notification(
    handle: *mut Handle,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    if handle.is_null() {
        set_error(error_out, "null mobile client handle");
        return ptr::null_mut();
    }
    // SAFETY: checked non-null and the handle remains owned by caller.
    let handle = unsafe { &*handle };
    match next_notification_json(handle) {
        Ok(Some(notification)) => CString::new(notification).unwrap().into_raw(),
        Ok(None) => ptr::null_mut(),
        Err(error) => {
            set_error(error_out, error);
            ptr::null_mut()
        }
    }
}

/// Returns one queued Host-initiated request, if available. The returned
/// JSON contains the raw request, including its numeric or string `id`.
/// Call `mobile_client_respond_result` or `mobile_client_respond_error`
/// with that ID to complete it.
///
/// # Safety
/// `handle` must remain live for the call. If non-null, `error_out` must
/// be writable for one `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_next_server_request(
    handle: *mut Handle,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    if handle.is_null() {
        set_error(error_out, "null mobile client handle");
        return ptr::null_mut();
    }
    // SAFETY: checked non-null and the handle remains owned by caller.
    let handle = unsafe { &*handle };
    match next_server_request_json(handle) {
        Ok(Some(request)) => CString::new(request).unwrap().into_raw(),
        Ok(None) => ptr::null_mut(),
        Err(error) => {
            set_error(error_out, error);
            ptr::null_mut()
        }
    }
}

/// Responds successfully to a Host-initiated request. `request_id_json`
/// must be a JSON number or string, and `result_json` may be any JSON
/// value. Returns 1 on success and 0 on failure.
///
/// # Safety
/// `handle` must remain live and every string pointer must designate a
/// NUL-terminated UTF-8 string. If non-null, `error_out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_respond_result(
    handle: *mut Handle,
    request_id_json: *const c_char,
    result_json: *const c_char,
    error_out: *mut *mut c_char,
) -> i32 {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    if handle.is_null() {
        set_error(error_out, "null mobile client handle");
        return 0;
    }
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let request_id = input_string(request_id_json)?;
        let result = input_string(result_json)?;
        // SAFETY: checked non-null and the handle remains owned by caller.
        let handle = unsafe { &*handle };
        respond_result_json(handle, request_id, result)
    }));
    match result {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            set_error(error_out, error);
            0
        }
        Err(_) => {
            set_error(error_out, "mobile client panicked");
            0
        }
    }
}

/// Responds with a structured RPC error to a Host-initiated request.
/// `error_json` must contain the raw error object, including `code`,
/// `message`, and optional `data`. Returns 1 on success and 0 on failure.
///
/// # Safety
/// `handle` must remain live and every string pointer must designate a
/// NUL-terminated UTF-8 string. If non-null, `error_out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_respond_error(
    handle: *mut Handle,
    request_id_json: *const c_char,
    error_json: *const c_char,
    error_out: *mut *mut c_char,
) -> i32 {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    if handle.is_null() {
        set_error(error_out, "null mobile client handle");
        return 0;
    }
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let request_id = input_string(request_id_json)?;
        let error = input_string(error_json)?;
        // SAFETY: checked non-null and the handle remains owned by caller.
        let handle = unsafe { &*handle };
        respond_error_json(handle, request_id, error)
    }));
    match result {
        Ok(Ok(())) => 1,
        Ok(Err(error)) => {
            set_error(error_out, error);
            0
        }
        Err(_) => {
            set_error(error_out, "mobile client panicked");
            0
        }
    }
}

/// Closes and destroys an opaque handle.
///
/// # Safety
/// `handle` must originate from `mobile_client_connect` and be passed
/// exactly once, after all concurrent calls that borrow it have returned.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_close(handle: *mut Handle) {
    close_handle(handle);
}

/// Releases a string returned by this C ABI.
///
/// # Safety
/// `value` must be null or an unmodified pointer returned by this crate's
/// string-returning C ABI functions, and must be freed exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_string_free(value: *mut c_char) {
    if !value.is_null() {
        // SAFETY: returned strings are allocated with CString::into_raw.
        drop(unsafe { CString::from_raw(value) });
    }
}
