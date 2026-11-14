use std::path::Path;

use reqwest::Client;
use serde_json::{Value, json};
use serde_json_path::JsonPath;

pub const DEFAULT_EXPRESSION: &str = "$['mainKey','dynamicKeys']";

pub fn is_endpoint(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

pub async fn load(client: &Client, source: &str, expression: &str) -> Result<Value, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("Keys source cannot be empty".into());
    }
    let bytes = if is_endpoint(source) {
        super::fetch_bytes(client, source).await?
    } else {
        let path = shellexpand::tilde(source);
        if Path::new(path.as_ref()).is_dir() {
            return Err("Keys filepath must point to a JSON file".into());
        }
        tokio::fs::read(Path::new(path.as_ref()))
            .await
            .map_err(|e| format!("Cannot read keys file: {e}"))?
    };
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    let document: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid keys JSON: {e}"))?;
    extract(&document, expression)
}

pub fn extract(document: &Value, expression: &str) -> Result<Value, String> {
    let path = JsonPath::parse(expression)
        .map_err(|e| format!("Invalid keys JSONPath expression: {e}"))?;
    let nodes = path.query(document).all();
    if nodes.is_empty() {
        return Err("The keys expression matched no values".into());
    }
    let mut main = None;
    let mut dynamic = Vec::new();
    fn collect(
        node: &Value,
        main: &mut Option<String>,
        dynamic: &mut Vec<Value>,
    ) -> Result<(), String> {
        match node {
            Value::String(key) => {
                if main.as_ref().is_some_and(|old| old != key) {
                    return Err("The expression matched multiple main keys".into());
                }
                *main = Some(key.clone());
            }
            Value::Array(entries) => {
                for entry in entries {
                    collect(entry, main, dynamic)?;
                }
            }
            Value::Object(object) => {
                if object.contains_key("guid") && object.contains_key("key") {
                    dynamic.push(node.clone());
                } else {
                    if let Some(value) = object.get("mainKey") {
                        collect(value, main, dynamic)?;
                    }
                    if let Some(value) = object.get("dynamicKeys") {
                        collect(value, main, dynamic)?;
                    }
                }
            }
            Value::Null => {}
            _ => {
                return Err(
                    "The expression must select a main key, dynamic key list, or keys object"
                        .into(),
                );
            }
        }
        Ok(())
    }
    for node in nodes {
        collect(node, &mut main, &mut dynamic)?;
    }
    let validate_key = |value: &str| {
        let value = value
            .strip_prefix("0x")
            .or_else(|| value.strip_prefix("0X"))
            .unwrap_or(value);
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
    };
    if main.as_deref().is_some_and(|key| !validate_key(key)) {
        return Err("The main AES key must contain 64 hexadecimal digits".into());
    }
    for entry in &dynamic {
        let guid = entry
            .get("guid")
            .and_then(Value::as_str)
            .unwrap_or("")
            .replace('-', "");
        if guid.len() != 32
            || !guid.bytes().all(|b| b.is_ascii_hexdigit())
            || !entry
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(validate_key)
        {
            return Err("Invalid dynamic AES key or GUID in expression results".into());
        }
    }
    if main.is_none() && dynamic.is_empty() {
        return Err("The expression did not select any AES keys".into());
    }
    Ok(json!({"mainKey":main,"dynamicKeys":dynamic}))
}
