//! JNI entry points used by Android's Kotlin/JVM transport wrapper.

use std::ptr;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jlong, jstring},
};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use zeroize::Zeroizing;

use crate::ffi::{self, CConfig};

fn exception(env: &mut JNIEnv<'_>, error: impl AsRef<str>) {
    let _ = env.throw_new("java/lang/RuntimeException", error.as_ref());
}

fn java_string(env: &mut JNIEnv<'_>, value: impl AsRef<str>) -> jstring {
    match env.new_string(value.as_ref()) {
        Ok(value) => value.into_raw(),
        Err(error) => {
            exception(env, error.to_string());
            ptr::null_mut()
        }
    }
}

fn string(env: &mut JNIEnv<'_>, value: JString<'_>) -> Result<String, String> {
    env.get_string(&value)
        .map_err(|error| error.to_string())?
        .to_str()
        .map(str::to_owned)
        .map_err(|error| error.to_string())
}

fn borrowed_handle(handle: jlong) -> Result<&'static ffi::Handle, String> {
    if handle == 0 {
        return Err("null mobile client handle".to_owned());
    }
    // SAFETY: Kotlin receives the pointer solely from connect and must not
    // use it after close; methods borrow it only for this native call.
    Ok(unsafe { &*(handle as *mut ffi::Handle) })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_generateDeviceKey(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jstring {
    match Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()) {
        Ok(key) => java_string(&mut env, URL_SAFE_NO_PAD.encode(key.as_ref())),
        Err(_) => {
            exception(&mut env, "secure random generation failed");
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_connect(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    config_json: JString<'_>,
    key_base64: JString<'_>,
) -> jlong {
    let result = (|| {
        let config: CConfig = serde_json::from_str(&string(&mut env, config_json)?)
            .map_err(|_| "invalid config JSON".to_owned())?;
        let key = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(string(&mut env, key_base64)?)
                .map_err(|_| "invalid device key base64".to_owned())?,
        );
        ffi::connect_handle(config, &key).map(|handle| handle as jlong)
    })();
    match result {
        Ok(handle) => handle,
        Err(error) => {
            exception(&mut env, error);
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_request(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    method: JString<'_>,
    params_json: JString<'_>,
) -> jstring {
    let result = (|| {
        let params = serde_json::from_str(&string(&mut env, params_json)?)
            .map_err(|_| "invalid params JSON".to_owned())?;
        ffi::request_json(borrowed_handle(handle)?, string(&mut env, method)?, params)
    })();
    match result {
        Ok(response) => java_string(&mut env, response),
        Err(error) => {
            exception(&mut env, error);
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_nextServerRequest(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    match borrowed_handle(handle).and_then(ffi::next_server_request_json) {
        Ok(Some(request)) => java_string(&mut env, request),
        Ok(None) => ptr::null_mut(),
        Err(error) => {
            exception(&mut env, error);
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_respondResult(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    request_id_json: JString<'_>,
    result_json: JString<'_>,
) -> jboolean {
    match (
        borrowed_handle(handle),
        string(&mut env, request_id_json),
        string(&mut env, result_json),
    ) {
        (Ok(handle), Ok(request_id), Ok(result)) => {
            match ffi::respond_result_json(handle, &request_id, &result) {
                Ok(()) => JNI_TRUE,
                Err(error) => {
                    exception(&mut env, error);
                    JNI_FALSE
                }
            }
        }
        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
            exception(&mut env, error);
            JNI_FALSE
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_respondError(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    request_id_json: JString<'_>,
    error_json: JString<'_>,
) -> jboolean {
    match (
        borrowed_handle(handle),
        string(&mut env, request_id_json),
        string(&mut env, error_json),
    ) {
        (Ok(handle), Ok(request_id), Ok(error_json)) => {
            match ffi::respond_error_json(handle, &request_id, &error_json) {
                Ok(()) => JNI_TRUE,
                Err(error) => {
                    exception(&mut env, error);
                    JNI_FALSE
                }
            }
        }
        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
            exception(&mut env, error);
            JNI_FALSE
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_nextNotification(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    match borrowed_handle(handle).and_then(ffi::next_notification_json) {
        Ok(Some(notification)) => java_string(&mut env, notification),
        Ok(None) => ptr::null_mut(),
        Err(error) => {
            exception(&mut env, error);
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_close(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) {
    if handle != 0 {
        // SAFETY: Kotlin calls close exactly once for the returned handle.
        ffi::close_handle(handle as *mut ffi::Handle);
    }
}
