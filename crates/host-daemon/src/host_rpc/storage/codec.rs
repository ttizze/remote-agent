//! Indexed fields live in SQL columns, not in a second copy inside the body.
//! The enum payload keeps the protocol's named MessagePack representation;
//! its discriminant is restored from the state column before deserialization.
use super::*;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

fn object(value: &mut Value) -> Result<&mut Map<String, Value>, String> {
    value
        .as_object_mut()
        .ok_or_else(|| "invalid stored record".into())
}

fn separate(value: Value) -> Result<(String, Value), String> {
    match value {
        Value::String(kind) => Ok((kind, Value::Null)),
        Value::Object(fields) if fields.len() == 1 => {
            Ok(fields.into_iter().next().expect("one variant"))
        }
        _ => Err("invalid stored state".into()),
    }
}

pub(super) fn enum_parts(value: &impl Serialize) -> Result<(String, Vec<u8>), String> {
    let (kind, payload) = separate(serde_json::to_value(value).map_err(failure)?)?;
    Ok((kind, protocol::encode(payload).map_err(failure)?))
}

pub(super) fn enum_record<T: DeserializeOwned>(kind: &str, bytes: &[u8]) -> Result<T, String> {
    let payload = protocol::decode(bytes).map_err(failure)?;
    serde_json::from_value(variant(kind, payload)).map_err(failure)
}

fn variant(kind: &str, payload: Value) -> Value {
    if payload.is_null() {
        Value::String(kind.into())
    } else {
        Value::Object(Map::from_iter([(kind.into(), payload)]))
    }
}

pub(super) fn pack(
    record: &impl Serialize,
    columns: &[&str],
    state: bool,
) -> Result<(String, Vec<u8>), String> {
    let mut body = serde_json::to_value(record).map_err(failure)?;
    let fields = object(&mut body)?;
    for column in columns {
        fields.remove(*column).ok_or("missing indexed field")?;
    }
    let kind = if state {
        let (kind, payload) = separate(fields.remove("state").ok_or("missing state")?)?;
        fields.insert("state".into(), payload);
        kind
    } else {
        String::new()
    };
    Ok((kind, protocol::encode(body).map_err(failure)?))
}

pub(super) fn unpack<T: DeserializeOwned>(
    bytes: &[u8],
    columns: impl IntoIterator<Item = (&'static str, Value)>,
    state: Option<&str>,
) -> Result<T, String> {
    let mut body: Value = protocol::decode(bytes).map_err(failure)?;
    let fields = object(&mut body)?;
    for (key, value) in columns {
        if fields.insert(key.into(), value).is_some() {
            return Err("indexed field duplicated in stored body".into());
        }
    }
    if let Some(kind) = state {
        let payload = fields.remove("state").ok_or("missing stored state")?;
        fields.insert("state".into(), variant(kind, payload));
    }
    serde_json::from_value(body).map_err(failure)
}

pub(super) fn header(header: &Header, counters: &Counters) -> Result<Vec<u8>, String> {
    let mut body = serde_json::to_value(header).map_err(failure)?;
    let fields = object(&mut body)?;
    let state = object(fields.get_mut("state").ok_or("missing thread state")?)?;
    let (kind, _) = separate(state.remove("workspace").ok_or("missing workspace")?)?;
    state.insert("workspace".into(), Value::String(kind));
    let details = object(fields.get_mut("details").ok_or("missing details")?)?;
    details.remove("parent");
    details.remove("updated_at");
    fields.insert(
        "counters".into(),
        serde_json::to_value(counters).map_err(failure)?,
    );
    protocol::encode(body).map_err(failure)
}

pub(super) fn read_header(
    bytes: &[u8],
    cwd: Option<&str>,
    parent: Option<SessionRef>,
    updated_at: i64,
) -> Result<(Header, Counters), String> {
    let mut body: Value = protocol::decode(bytes).map_err(failure)?;
    let fields = object(&mut body)?;
    let counters = serde_json::from_value(fields.remove("counters").ok_or("missing counters")?)
        .map_err(failure)?;
    let state = object(fields.get_mut("state").ok_or("missing thread state")?)?;
    let kind = state
        .get("workspace")
        .and_then(Value::as_str)
        .ok_or("missing workspace state")?
        .to_owned();
    let payload = match kind.as_str() {
        "missing" if cwd.is_none() => Value::Null,
        "folder" | "ready" => Value::String(cwd.ok_or("missing working directory")?.into()),
        _ => return Err("invalid working directory state".into()),
    };
    state.insert("workspace".into(), variant(&kind, payload));
    let details = object(fields.get_mut("details").ok_or("missing details")?)?;
    details.insert(
        "parent".into(),
        serde_json::to_value(parent).map_err(failure)?,
    );
    details.insert("updated_at".into(), updated_at.into());
    Ok((serde_json::from_value(body).map_err(failure)?, counters))
}
