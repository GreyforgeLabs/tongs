//! Dotted key paths (`service.port`, `tags.1`) over JSON documents, with the
//! same semantics as the Python 1.x CLI: list indexes accept anything
//! Python's `int()` accepts, including negative indexes from the end.

use super::repr as str_repr;
use tongs::python::parse_int;
use tongs::{Map, Value};

/// A dotted key path does not resolve inside the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPathError(pub String);

impl std::fmt::Display for KeyPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

fn split_path(key: &str) -> Result<Vec<&str>, KeyPathError> {
    let parts: Vec<&str> = if key.is_empty() {
        Vec::new()
    } else {
        key.split('.').collect()
    };
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err(KeyPathError(format!("invalid key path {}", str_repr(key))));
    }
    Ok(parts)
}

fn not_found(key: &str) -> KeyPathError {
    KeyPathError(format!("key path {} not found", str_repr(key)))
}

fn scalar(key: &str) -> KeyPathError {
    KeyPathError(format!(
        "key path {} does not resolve through a scalar",
        str_repr(key)
    ))
}

/// Python list indexing: `int(part)`, negative counts from the end.
fn list_index(part: &str, len: usize) -> Option<usize> {
    let n = parse_int(part)?;
    let idx = if n < 0 { n + len as i128 } else { n };
    (0..len as i128).contains(&idx).then_some(idx as usize)
}

fn descend<'v>(node: &'v Value, part: &str, key: &str) -> Result<&'v Value, KeyPathError> {
    match node {
        Value::Object(map) => map.get(part).ok_or_else(|| not_found(key)),
        Value::Array(items) => list_index(part, items.len())
            .map(|i| &items[i])
            .ok_or_else(|| not_found(key)),
        _ => Err(scalar(key)),
    }
}

fn descend_mut<'v>(
    node: &'v mut Value,
    part: &str,
    key: &str,
) -> Result<&'v mut Value, KeyPathError> {
    match node {
        Value::Object(map) => map.get_mut(part).ok_or_else(|| not_found(key)),
        Value::Array(items) => {
            let len = items.len();
            list_index(part, len)
                .map(move |i| &mut items[i])
                .ok_or_else(|| not_found(key))
        }
        _ => Err(scalar(key)),
    }
}

/// The value at `key`.
pub fn get_path<'v>(data: &'v Value, key: &str) -> Result<&'v Value, KeyPathError> {
    let mut node = data;
    for part in split_path(key)? {
        node = descend(node, part, key)?;
    }
    Ok(node)
}

/// Write `value` at `key`, creating missing (or `null`) intermediate objects.
pub fn set_path(data: &mut Value, key: &str, value: Value) -> Result<(), KeyPathError> {
    let parts = split_path(key)?;
    let (last, parents) = parts.split_last().expect("split_path is never empty");
    let mut node = data;
    for part in parents {
        node = match node {
            Value::Object(map) => {
                if map.get(*part).is_none_or(Value::is_null) {
                    map.insert((*part).to_owned(), Value::Object(Map::new()));
                }
                map.get_mut(*part).expect("just inserted")
            }
            Value::Array(_) => descend_mut(node, part, key)?,
            _ => return Err(scalar(key)),
        };
    }
    match node {
        Value::Object(map) => {
            map.insert((*last).to_owned(), value);
            Ok(())
        }
        Value::Array(items) => {
            let i = list_index(last, items.len()).ok_or_else(|| not_found(key))?;
            items[i] = value;
            Ok(())
        }
        _ => Err(scalar(key)),
    }
}

/// Remove the value at `key` (object order is preserved).
pub fn delete_path(data: &mut Value, key: &str) -> Result<(), KeyPathError> {
    let parts = split_path(key)?;
    let (last, parents) = parts.split_last().expect("split_path is never empty");
    let mut node = data;
    for part in parents {
        node = descend_mut(node, part, key)?;
    }
    match node {
        Value::Object(map) => map
            .shift_remove(*last)
            .map(|_| ())
            .ok_or_else(|| not_found(key)),
        Value::Array(items) => {
            let i = list_index(last, items.len()).ok_or_else(|| not_found(key))?;
            items.remove(i);
            Ok(())
        }
        _ => Err(scalar(key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tongs::json;

    // Port of tests/test_cli.py::test_path_helpers.
    #[test]
    fn path_helpers() {
        let mut data = json!({"a": {"b": [10, {"c": 1}]}});
        assert_eq!(get_path(&data, "a.b.1.c").unwrap(), &json!(1));
        set_path(&mut data, "a.b.0", json!(11)).unwrap();
        set_path(&mut data, "x.y.z", json!("new")).unwrap();
        assert_eq!(data["a"]["b"][0], json!(11));
        assert_eq!(data["x"], json!({"y": {"z": "new"}}));
        delete_path(&mut data, "x.y").unwrap();
        assert_eq!(data["x"], json!({}));
        for bad in ["", "a..b", "a.b.9", "a.b.0.c", "missing.key"] {
            assert!(get_path(&data, bad).is_err(), "{bad}");
        }
        assert!(set_path(&mut data, "a.b.0.c", json!(1)).is_err());
        assert!(delete_path(&mut data, "a.b.9").is_err());
    }

    #[test]
    fn python_index_semantics_and_messages() {
        let mut data = json!({"tags": ["a", "b", "c"], "n": null, "s": 5});
        assert_eq!(get_path(&data, "tags.-1").unwrap(), &json!("c"));
        assert_eq!(get_path(&data, "tags. 1").unwrap(), &json!("b"));
        assert_eq!(get_path(&data, "tags.+0").unwrap(), &json!("a"));
        set_path(&mut data, "n.k", json!(1)).unwrap();
        assert_eq!(data["n"], json!({"k": 1}));
        assert_eq!(
            get_path(&data, "a..b").unwrap_err().0,
            "invalid key path 'a..b'"
        );
        assert_eq!(
            get_path(&data, "zz").unwrap_err().0,
            "key path 'zz' not found"
        );
        assert_eq!(
            set_path(&mut data, "s.x", json!(1)).unwrap_err().0,
            "key path 's.x' does not resolve through a scalar"
        );
        delete_path(&mut data, "tags.0").unwrap();
        assert_eq!(data["tags"], json!(["b", "c"]));
        let mut ordered = json!({"a": 1, "b": 2, "c": 3});
        delete_path(&mut ordered, "a").unwrap();
        assert_eq!(
            ordered.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["b", "c"]
        );
    }
}
