use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: Id,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Id,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
}

/// A JSON-RPC 2.0 message: request, response, or notification.
#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl Message {
    /// Parse a JSON value into a Message, distinguishing requests, responses,
    /// and notifications by their fields.
    pub fn parse(value: Value) -> Result<Self, serde_json::Error> {
        let obj = value.as_object();

        // Response: has "id" but no "method"
        if obj.is_some_and(|o| o.contains_key("id") && !o.contains_key("method")) {
            return Ok(Message::Response(serde_json::from_value(value)?));
        }

        // Request: has both "id" and "method"
        if obj.is_some_and(|o| o.contains_key("id") && o.contains_key("method")) {
            return Ok(Message::Request(serde_json::from_value(value)?));
        }

        // Notification: has "method" but no "id"
        Ok(Message::Notification(serde_json::from_value(value)?))
    }
}

impl Request {
    pub fn new(id: impl Into<Id>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

impl Response {
    pub fn ok(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Id, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(ResponseError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

impl From<i64> for Id {
    fn from(n: i64) -> Self {
        Id::Number(n)
    }
}

impl From<String> for Id {
    fn from(s: String) -> Self {
        Id::String(s)
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Id::Number(n) => write!(f, "{n}"),
            Id::String(s) => write!(f, "{s}"),
        }
    }
}

impl Serialize for Message {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Message::Request(r) => r.serialize(serializer),
            Message::Response(r) => r.serialize(serializer),
            Message::Notification(n) => n.serialize(serializer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_request() {
        let val = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"capabilities": {}}
        });
        let msg = Message::parse(val).unwrap();
        assert!(matches!(msg, Message::Request(r) if r.method == "initialize"));
    }

    #[test]
    fn parse_response() {
        let val = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"capabilities": {}}
        });
        let msg = Message::parse(val).unwrap();
        assert!(matches!(msg, Message::Response(r) if r.result.is_some()));
    }

    #[test]
    fn parse_notification() {
        let val = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {"uri": "file:///test.v", "diagnostics": []}
        });
        let msg = Message::parse(val).unwrap();
        assert!(matches!(msg, Message::Notification(n) if n.method == "textDocument/publishDiagnostics"));
    }

    #[test]
    fn parse_error_response() {
        let val = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {"code": -32600, "message": "Invalid Request"}
        });
        let msg = Message::parse(val).unwrap();
        assert!(matches!(msg, Message::Response(r) if r.error.is_some()));
    }

    #[test]
    fn request_serialization_roundtrip() {
        let req = Request::new(1i64, "test/method", Some(json!({"key": "value"})));
        let json_str = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.method, "test/method");
        assert_eq!(parsed.id, Id::Number(1));
    }
}
