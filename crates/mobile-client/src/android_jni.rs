//! JNI entry points used by Android's Kotlin/JVM transport wrapper.

use std::ptr;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use jni::{
    JNIEnv,
    objects::{JByteArray, JClass, JString},
    sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong, jstring},
};
use ring::{rand::SystemRandom, signature::Ed25519KeyPair};
use zeroize::Zeroizing;

use crate::ffi::{self, CConfig};

fn exception(env: &mut JNIEnv<'_>, error: impl AsRef<str>) {
    let _ = env.throw_new("java/lang/IllegalStateException", error.as_ref());
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
    device_pkcs8: JByteArray<'_>,
) -> jlong {
    let result = (|| {
        let config: CConfig = serde_json::from_str(&string(&mut env, config_json)?)
            .map_err(|_| "invalid config JSON".to_owned())?;
        let key = Zeroizing::new(
            env.convert_byte_array(&device_pkcs8)
                .map_err(|_| "invalid device key bytes".to_owned())?,
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
        let method = string(&mut env, method)?;
        let params = string(&mut env, params_json)?;
        ffi::request_json(borrowed_handle(handle)?, method, params)
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
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_nextEvent(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
) -> jstring {
    match borrowed_handle(handle).and_then(ffi::next_event_json) {
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

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_transfer(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    params_json: JString<'_>,
) -> jstring {
    let result = (|| {
        let params = string(&mut env, params_json)?;
        ffi::transfer_json(borrowed_handle(handle)?, &params)
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
pub extern "system" fn Java_dev_remoteagent_mobile_NativeConversation_presentConversation(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    request_json: JString<'_>,
) -> jstring {
    match string(&mut env, request_json)
        .and_then(|request| conversation_presentation::present_json(&request))
    {
        Ok(result) => java_string(&mut env, result),
        Err(error) => {
            exception(&mut env, error);
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeConversation_classifyEvent(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    method: JString<'_>,
) -> jint {
    let code = env.get_string(&method).ok().and_then(|method| {
        method
            .to_str()
            .ok()
            .map(|method| conversation_presentation::state::classify_event(method, false) as jint)
    });
    code.unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeConversation_conversationTransition(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    kind: jint,
    status: jint,
    current_status: jint,
    item: jint,
    flags: jint,
) -> jint {
    conversation_presentation::state::transition_code(
        kind as u32,
        status as u32,
        current_status as u32,
        item as u32,
        flags as u32,
    ) as jint
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeConversation_accountTransition(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    event: jint,
    flags: jint,
) -> jint {
    agent_client::accounts::transition(event as u32, flags as u32) as jint
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remoteagent_mobile_NativeHostTransport_agentCommand(
    mut env: JNIEnv<'_>,
    _class: JClass<'_>,
    handle: jlong,
    command_json: JString<'_>,
) -> jstring {
    let result = (|| {
        let command = string(&mut env, command_json)?;
        ffi::agent_command_json(borrowed_handle(handle)?, &command)
    })();
    match result {
        Ok(response) => java_string(&mut env, response),
        Err(error) => {
            exception(&mut env, error);
            ptr::null_mut()
        }
    }
}
