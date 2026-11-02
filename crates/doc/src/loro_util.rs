//! Reads of Loro map fields. Writes go through the container handles directly.

use loro::{Container, LoroMap, LoroValue, ValueOrContainer};

pub(crate) fn map_str(map: &LoroMap, key: &str) -> String {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::String(value))) => value.to_string(),
        _ => String::new(),
    }
}

pub(crate) fn map_opt_str(map: &LoroMap, key: &str) -> Option<String> {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::String(value))) => Some(value.to_string()),
        _ => None,
    }
}

pub(crate) fn map_i64(map: &LoroMap, key: &str) -> i64 {
    map_opt_i64(map, key).unwrap_or(0)
}

/// Absent when the key was never written. `0` is a real value, not "missing".
pub(crate) fn map_opt_i64(map: &LoroMap, key: &str) -> Option<i64> {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::I64(value))) => Some(value),
        _ => None,
    }
}

pub(crate) fn map_bool(map: &LoroMap, key: &str) -> bool {
    match map.get(key) {
        Some(ValueOrContainer::Value(LoroValue::Bool(value))) => value,
        _ => false,
    }
}

pub(crate) fn map_u64(map: &LoroMap, key: &str) -> u64 {
    u64::try_from(map_i64(map, key)).unwrap_or(0)
}

pub(crate) fn as_map(value: ValueOrContainer) -> Option<LoroMap> {
    match value {
        ValueOrContainer::Container(Container::Map(map)) => Some(map),
        _ => None,
    }
}
