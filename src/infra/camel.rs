use axum::{body::Body, extract::Request, http::header, middleware::Next, response::Response};
use serde_json::{Map, Value};

/// Values under these keys are user-authored documents and must pass through untouched.
const USER_DOCS: [&str; 5] = ["noteDocument", "note_document", "document", "drawing", "scene"];

/// `item_type` -> `itemType`. Only lowercase snake_case is converted, so data keys such as
/// `PENDING` or `NOT_FOUND` are left alone.
fn camel_key(key: &str) -> String {
    let snake = key.contains('_')
        && key.starts_with(|c: char| c.is_ascii_lowercase())
        && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !key.ends_with('_')
        && !key.contains("__");
    if !snake {
        return key.to_string();
    }
    let mut out = String::with_capacity(key.len());
    let mut upper = false;
    for c in key.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

pub fn camelize(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let v = if USER_DOCS.contains(&k.as_str()) { v } else { camelize(v) };
                    (camel_key(&k), v)
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(camelize).collect()),
        other => other,
    }
}

/// Router layer: every JSON response leaves the API with camelCase keys, whatever built it
/// (typed structs, `to_jsonb(row)` or `json!`).
pub async fn camelize_json(req: Request, next: Next) -> Response {
    let resp = next.run(req).await;
    let is_json = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !is_json {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, 64 * 1024 * 1024).await else {
        return Response::builder().status(500).body(Body::empty()).unwrap();
    };
    let out = match serde_json::from_slice::<Value>(&bytes) {
        Ok(v) => serde_json::to_vec(&camelize(v)).unwrap_or_else(|_| bytes.to_vec()),
        Err(_) => bytes.to_vec(),
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn converts_api_keys_but_not_user_documents_or_data_keys() {
        let out = camelize(json!({
            "item_type": "LINK",
            "note_document": { "text_format": 1, "children": [{ "some_key": 2 }] },
            "tags": [{ "bookmark_count": 3 }],
            "counts": { "NOT_FOUND": 1, "PENDING": 2 },
        }));
        assert_eq!(out["itemType"], "LINK");
        assert_eq!(out["noteDocument"]["text_format"], 1);
        assert_eq!(out["noteDocument"]["children"][0]["some_key"], 2);
        assert_eq!(out["tags"][0]["bookmarkCount"], 3);
        assert_eq!(out["counts"]["NOT_FOUND"], 1);
    }
}
