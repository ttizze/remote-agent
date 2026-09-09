//! Minimal C ABI for Android JNI/Swift wrappers. The command/event model above
//! remains the primary API; this borrows the process runtime and never persists
//! or logs the caller-supplied PKCS#8 key.

use std::{
    collections::HashMap,
    ffi::{CStr, CString, c_char},
    panic::AssertUnwindSafe,
    ptr,
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use host_protocol::{Ed25519PublicKey, PairingToken, RelayEndpoint};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use serde::Deserialize;
use tokio::sync::broadcast;

use crate::{MobileClient, MobileClientConfig, MobileClientError};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CConfig {
    #[serde(flatten)]
    relay: RelayEndpoint,
    host_identity: Ed25519PublicKey,
    device_name: String,
    #[serde(default)]
    pairing_ticket: Option<PairingToken>,
    request_timeout_ms: u64,
}

impl From<CConfig> for MobileClientConfig {
    fn from(value: CConfig) -> Self {
        Self {
            relay: value.relay,
            host_identity: value.host_identity,
            device_name: value.device_name,
            pairing_ticket: value.pairing_ticket,
            request_timeout: Duration::from_millis(value.request_timeout_ms),
        }
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
    runtime: &'static tokio::runtime::Runtime,
    client: MobileClient,
    events: Mutex<broadcast::Receiver<String>>,
}

// Only native resources live in this registry. IDs never repeat, including after
// close, so racing or stale FFI calls cannot borrow a replacement connection.
static HANDLES: LazyLock<Mutex<HashMap<u64, Arc<Handle>>>> = LazyLock::new(Default::default);
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

pub(crate) fn borrow_handle(id: u64) -> Result<Arc<Handle>, String> {
    HANDLES
        .lock()
        .map_err(|_| "handle lock poisoned")?
        .get(&id)
        .cloned()
        .ok_or_else(|| "mobile client handle is closed".to_owned())
}

pub(crate) fn register_client(
    runtime: &'static tokio::runtime::Runtime,
    client: MobileClient,
) -> Result<u64, String> {
    let events = client.subscribe();
    let handle = Handle {
        runtime,
        client,
        events: Mutex::new(events),
    };
    let id = NEXT_HANDLE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .map_err(|_| "mobile client handles exhausted")?;
    HANDLES
        .lock()
        .map_err(|_| "handle lock poisoned")?
        .insert(id, Arc::new(handle));
    Ok(id)
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

pub(crate) fn connect_handle(config: CConfig, key: &[u8]) -> Result<u64, String> {
    let runtime = host_protocol::rpc_runtime()?;
    let client = runtime
        .block_on(MobileClient::connect(config.into(), key))
        .map_err(|error| error.to_string())?;
    register_client(runtime, client)
}

pub(crate) fn agent_command_json(handle: &Handle, command: &str) -> Result<String, String> {
    handle
        .runtime
        .block_on(handle.client.agent().command_json(command))
        .map_err(agent_client::operations::AgentError::into_native_error)
}

fn encode_mobile_error(error: MobileClientError) -> String {
    match error {
        MobileClientError::Agent(error) => error.into_native_error(),
        other => other.to_string(),
    }
}

pub(crate) fn next_event_json(handle: &Handle) -> Result<Option<String>, String> {
    let mut events = handle.events.lock().map_err(|_| "event lock poisoned")?;
    match events.try_recv() {
        Ok(event) => Ok(Some(event)),
        Err(broadcast::error::TryRecvError::Empty) => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

pub(crate) fn close_handle(id: u64) {
    // Release the registry reference outside its lock. Existing calls retain
    // their own Arc; the transport closes when the last call has returned.
    let retired = HANDLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
    drop(retired);
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
) -> u64 {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    let result = std::panic::catch_unwind(|| -> Result<u64, String> {
        let config: CConfig = serde_json::from_str(input_string(config_json)?)
            .map_err(|_| "invalid config JSON".to_owned())?;
        if device_pkcs8.is_null() || device_pkcs8_len == 0 {
            return Err("missing device PKCS#8 bytes".to_owned());
        }
        // SAFETY: caller promises a readable byte range for this call.
        let key = unsafe { std::slice::from_raw_parts(device_pkcs8, device_pkcs8_len) };
        connect_handle(config, key)
    });
    match result {
        Ok(Ok(handle)) => handle,
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

/// Executes a typed agent intent through the shared PC/mobile client.
///
/// # Safety
/// String inputs must be valid NUL-terminated UTF-8. If non-null,
/// `error_out` must be writable for one `char *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_agent_command(
    handle: u64,
    command_json: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<CString, String> {
        let command = input_string(command_json)?;
        let handle = borrow_handle(handle)?;
        CString::new(agent_command_json(&handle, command)?)
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

/// Returns the next notification or Host request in wire order, if available.
///
/// # Safety
/// If non-null, `error_out` must be writable for
/// one `char *`; non-null returned strings use `mobile_client_string_free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_next_event(
    handle: u64,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        // SAFETY: checked non-null and owned by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    match borrow_handle(handle).and_then(|handle| next_event_json(&handle)) {
        Ok(Some(notification)) => CString::new(notification).unwrap().into_raw(),
        Ok(None) => ptr::null_mut(),
        Err(error) => {
            set_error(error_out, error);
            ptr::null_mut()
        }
    }
}

/// Retires a handle. Concurrent calls finish before its transport is destroyed.
/// Repeated close and stale handle IDs are harmless.
#[unsafe(no_mangle)]
pub extern "C" fn mobile_client_close(handle: u64) {
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

pub(crate) fn transfer_json(handle: &Handle, params: &str) -> Result<String, String> {
    use std::path::PathBuf;
    #[derive(Deserialize)]
    #[serde(tag = "direction", rename_all = "camelCase")]
    enum Transfer {
        Upload {
            source: PathBuf,
            directory: PathBuf,
            #[serde(rename = "fileName")]
            file_name: String,
        },
        Download {
            source: PathBuf,
            destination: PathBuf,
        },
    }
    let params: Transfer =
        serde_json::from_str(params).map_err(|_| "invalid transfer parameters")?;
    let result = handle
        .runtime
        .block_on(async {
            match params {
                Transfer::Upload {
                    source,
                    directory,
                    file_name,
                } => {
                    handle
                        .client
                        .upload_file(&source, &directory, &file_name)
                        .await
                }
                Transfer::Download {
                    source,
                    destination,
                } => {
                    handle.client.download_file(&source, &destination).await?;
                    Ok(serde_json::json!({"path":destination}))
                }
            }
        })
        .map_err(encode_mobile_error)?;
    serde_json::to_string(&result).map_err(|_| "failed to encode transfer result".into())
}

/// Transfers a picked file over a dedicated encrypted channel.
///
/// # Safety
/// Inputs must be NUL-terminated UTF-8 and error_out, if non-null, writable
/// for one pointer. The call retains its handle through completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_transfer(
    handle: u64,
    params_json: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        unsafe {
            *error_out = ptr::null_mut();
        }
    }
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| -> Result<CString, String> {
        let params = input_string(params_json)?;
        let handle = borrow_handle(handle)?;
        CString::new(transfer_json(&handle, params)?)
            .map_err(|_| "transfer result contains NUL".into())
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

/// Projects conversation metadata through the shared desktop/mobile policy.
///
/// # Safety
/// `request_json` must be valid NUL-terminated UTF-8 for this call. If non-null,
/// `error_out` must be writable. Release returned strings with string_free.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_present_conversation(
    request_json: *const c_char,
    error_out: *mut *mut c_char,
) -> *mut c_char {
    if !error_out.is_null() {
        // SAFETY: guaranteed writable by the caller.
        unsafe { *error_out = ptr::null_mut() };
    }
    let result = std::panic::catch_unwind(|| {
        let result = conversation_presentation::present_json(input_string(request_json)?)?;
        CString::new(result).map_err(|e| e.to_string())
    });
    match result {
        Ok(Ok(value)) => value.into_raw(),
        Ok(Err(error)) => {
            set_error(error_out, error);
            ptr::null_mut()
        }
        Err(_) => {
            set_error(error_out, "conversation presentation panicked");
            ptr::null_mut()
        }
    }
}

/// Classifies a UTF-8 method without allocating JSON. Unknown/invalid methods
/// return zero; server requests return RequestStarted independently of method.
///
/// # Safety
/// A non-null method must point to a valid NUL-terminated string for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mobile_client_classify_event(
    method: *const c_char,
    is_request: i32,
) -> u32 {
    if method.is_null() {
        return 0;
    }
    // SAFETY: guaranteed by the C ABI caller.
    unsafe { CStr::from_ptr(method) }
        .to_str()
        .map(|method| {
            conversation_presentation::state::classify_event(method, is_request != 0) as u32
        })
        .unwrap_or(0)
}

/// Allocation-free transition; see mobile_client.h for the packed enum contract.
#[unsafe(no_mangle)]
pub extern "C" fn mobile_client_conversation_transition(
    kind: u32,
    status: u32,
    current_status: u32,
    item: u32,
    flags: u32,
) -> u32 {
    conversation_presentation::state::transition_code(kind, status, current_status, item, flags)
}

#[cfg(test)]
mod presentation_tests {
    use super::*;
    #[test]
    fn presentation_ffi_returns_owned_utf8_and_reports_invalid_input() {
        let request = CString::new(r#"{"operation":"item","item":{"type":"reasoning"}}"#).unwrap();
        let mut error = ptr::null_mut();
        // SAFETY: all inputs and output pointers remain valid, each returned
        // allocation is freed exactly once through its matching ABI.
        unsafe {
            let result = mobile_client_present_conversation(request.as_ptr(), &mut error);
            assert!(error.is_null());
            assert!(!result.is_null());
            let value: serde_json::Value =
                serde_json::from_str(CStr::from_ptr(result).to_str().unwrap()).unwrap();
            assert_eq!(value["title"], "思考");
            mobile_client_string_free(result);
            let invalid = [0xffu8, 0];
            let malformed = CString::new("{").unwrap();
            for input in [ptr::null(), invalid.as_ptr().cast(), malformed.as_ptr()] {
                let result = mobile_client_present_conversation(input, &mut error);
                assert!(result.is_null());
                assert!(!error.is_null());
                assert!(!CStr::from_ptr(error).to_bytes().is_empty());
                mobile_client_string_free(error);
            }
            assert!(mobile_client_present_conversation(ptr::null(), ptr::null_mut()).is_null());
        }
    }
}
