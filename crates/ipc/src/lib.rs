//! JSON-RPC 2.0 IPC layer for PRISM.
//!
//! Bridges the Rust CLI process and the Ink TUI frontend over stdin/stdout.
//! The [`IpcServer`] spawns the TUI binary as a child process, then sends and
//! receives typed JSON-RPC messages through piped stdio channels.
//!
//! Wire types: [`RpcRequest`], [`RpcResponse`], [`RpcNotification`], [`RpcError`].

pub mod server;
pub mod methods;
pub mod types;

pub use server::IpcServer;
pub use types::{RpcRequest, RpcResponse, RpcNotification, RpcError};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_request_serialization() {
        let req = RpcRequest::new("ipc.hello", serde_json::json!({"version": "2.5.0"}), 1);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("ipc.hello"));
        assert!(json.contains("2.0"));

        let parsed: RpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "ipc.hello");
        assert_eq!(parsed.id, Some(1));
    }

    #[test]
    fn rpc_response_ok() {
        let resp = RpcResponse::ok(Some(1), serde_json::json!({"status": "ok"}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("status"));
        assert!(!json.contains("error"));
    }

    #[test]
    fn rpc_response_error() {
        let resp = RpcResponse::err(Some(1), -32600, "Invalid request".into());
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("error"));
        assert!(json.contains("-32600"));
        assert!(!json.contains("result"));
    }

    #[test]
    fn rpc_notification() {
        let notif = RpcNotification::new("ui.text.delta", serde_json::json!({"text": "hello"}));
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("ui.text.delta"));
        // Notifications have no id
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.get("id").is_none());
    }

    // ── New edge-case tests ─────────────────────────────────────────

    #[test]
    fn rpc_request_with_null_params_default() {
        // When params is omitted in JSON, it should default to serde_json::Value::Null
        let json = r#"{"jsonrpc":"2.0","method":"ping","id":7}"#;
        let req: RpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "ping");
        assert_eq!(req.id, Some(7));
        // Default for serde_json::Value with #[serde(default)] is Null
        assert_eq!(req.params, serde_json::Value::Null);
    }

    #[test]
    fn rpc_request_notification_style_no_id() {
        // A notification-style request has id = null / absent
        let json = r#"{"jsonrpc":"2.0","method":"notify.event","params":{}}"#;
        let req: RpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, None);
        assert_eq!(req.method, "notify.event");
    }

    #[test]
    fn rpc_response_ok_with_none_id() {
        let resp = RpcResponse::ok(None, serde_json::json!({"status": "ok"}));
        let json = serde_json::to_string(&resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        // id field is present but null
        assert!(v.get("id").is_some());
        assert!(v["id"].is_null());
        assert!(v.get("error").is_none());
        assert!(v.get("result").is_some());
    }

    #[test]
    fn rpc_response_err_with_none_id() {
        let resp = RpcResponse::err(None, -32600, "Invalid request".into());
        let json = serde_json::to_string(&resp).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["id"].is_null());
        assert!(v.get("result").is_none());
        assert_eq!(v["error"]["code"], -32600);
    }

    #[test]
    fn rpc_response_roundtrip_preserves_all_fields() {
        let resp = RpcResponse::ok(Some(42), serde_json::json!({"data": [1, 2, 3]}));
        let json = serde_json::to_string(&resp).unwrap();
        let back: RpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(back.jsonrpc, "2.0");
        assert_eq!(back.id, Some(42));
        assert!(back.error.is_none());
        assert_eq!(back.result.unwrap()["data"][1], 2);
    }

    #[test]
    fn rpc_notification_empty_params() {
        // params defaults to Null when omitted; empty object also valid
        let notif = RpcNotification::new("heartbeat", serde_json::json!({}));
        let json = serde_json::to_string(&notif).unwrap();
        let back: RpcNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(back.method, "heartbeat");
        assert_eq!(back.params, serde_json::json!({}));
    }

    #[test]
    fn rpc_error_serde_roundtrip() {
        let err = RpcError {
            code: -32603,
            message: "Internal error".into(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: RpcError = serde_json::from_str(&json).unwrap();
        assert_eq!(back.code, -32603);
        assert_eq!(back.message, "Internal error");
    }

    #[test]
    fn standard_json_rpc_error_codes() {
        // Verify the four standard parse/request/method/params/internal codes
        let cases: &[(i32, &str)] = &[
            (-32700, "Parse error"),
            (-32600, "Invalid Request"),
            (-32601, "Method not found"),
            (-32602, "Invalid params"),
            (-32603, "Internal error"),
        ];
        for &(code, message) in cases {
            let resp = RpcResponse::err(Some(1), code, message.into());
            let json = serde_json::to_string(&resp).unwrap();
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(
                v["error"]["code"], code,
                "code mismatch for {message}"
            );
            assert_eq!(
                v["error"]["message"], message,
                "message mismatch for code {code}"
            );
        }
    }
}
