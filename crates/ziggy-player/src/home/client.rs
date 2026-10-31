//! Home Assistant's WebSocket messages: what Ziggy sends and what it reads back.

use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    AuthRequired,
    AuthOk,
    AuthInvalid(String),
    /// The answer to a command; `error` is empty when `ok`.
    Result {
        id: u64,
        ok: bool,
        result: Value,
        error: String,
    },
    /// An event of a subscription, by the subscribing command's id.
    Event {
        id: u64,
        event: Value,
    },
    Pong,
    Other,
}

pub fn parse(text: &str) -> Frame {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Frame::Other;
    };
    let id = v["id"].as_u64().unwrap_or(0);
    match v["type"].as_str() {
        Some("auth_required") => Frame::AuthRequired,
        Some("auth_ok") => Frame::AuthOk,
        Some("auth_invalid") => {
            Frame::AuthInvalid(v["message"].as_str().unwrap_or_default().into())
        }
        Some("result") => Frame::Result {
            id,
            ok: v["success"].as_bool().unwrap_or(false),
            result: v["result"].clone(),
            error: v["error"]["message"].as_str().unwrap_or_default().into(),
        },
        Some("event") => Frame::Event {
            id,
            event: v["event"].clone(),
        },
        Some("pong") => Frame::Pong,
        _ => Frame::Other,
    }
}

pub fn auth(token: &str) -> String {
    json!({"type": "auth", "access_token": token}).to_string()
}

pub fn subscribe(id: u64, entity_ids: &[String]) -> String {
    json!({"id": id, "type": "subscribe_entities", "entity_ids": entity_ids}).to_string()
}

/// A command without arguments, such as `config/area_registry/list`.
pub fn command(id: u64, kind: &str) -> String {
    json!({"id": id, "type": kind}).to_string()
}

pub fn ping(id: u64) -> String {
    command(id, "ping")
}

/// Calls `<domain>.<service>` on one entity, with a brightness for `light.turn_on`.
pub fn call(id: u64, entity_id: &str, service: &str, brightness_pct: Option<u8>) -> String {
    let domain = entity_id.split_once('.').map_or(entity_id, |(d, _)| d);
    let mut v = json!({
        "id": id,
        "type": "call_service",
        "domain": domain,
        "service": service,
        "target": {"entity_id": entity_id},
    });
    if let Some(pct) = brightness_pct {
        v["service_data"] = json!({"brightness_pct": pct});
    }
    v.to_string()
}

/// Config checks made sure the address is `http://`.
pub fn websocket_url(base: &str) -> String {
    let host = base.trim_start_matches("http://").trim_end_matches('/');
    format!("ws://{host}/api/websocket")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn frames_parse() {
        assert_eq!(
            parse(r#"{"type":"auth_required","ha_version":"2026.2.3"}"#),
            Frame::AuthRequired
        );
        assert_eq!(parse(r#"{"type":"auth_ok"}"#), Frame::AuthOk);
        assert_eq!(
            parse(r#"{"type":"auth_invalid","message":"Invalid access token"}"#),
            Frame::AuthInvalid("Invalid access token".into())
        );
        assert_eq!(
            parse(
                r#"{"id":7,"type":"result","success":false,"error":{"code":"not_found","message":"Entity not found"}}"#
            ),
            Frame::Result {
                id: 7,
                ok: false,
                result: Value::Null,
                error: "Entity not found".into()
            }
        );
        assert_eq!(
            parse(r#"{"id":1,"type":"event","event":{"r":["light.x"]}}"#),
            Frame::Event {
                id: 1,
                event: json!({"r": ["light.x"]})
            }
        );
        assert_eq!(parse(r#"{"id":9,"type":"pong"}"#), Frame::Pong);
        assert_eq!(parse("not json"), Frame::Other);
    }

    #[test]
    fn messages_are_what_home_assistant_expects() {
        let v = |s: String| serde_json::from_str::<Value>(&s).unwrap();
        assert_eq!(
            v(auth("tok")),
            json!({"type": "auth", "access_token": "tok"})
        );
        assert_eq!(
            v(subscribe(1, &["light.a".to_string()])),
            json!({"id": 1, "type": "subscribe_entities", "entity_ids": ["light.a"]})
        );
        assert_eq!(
            v(call(5, "light.desk", "turn_on", Some(70))),
            json!({"id": 5, "type": "call_service", "domain": "light", "service": "turn_on",
                   "target": {"entity_id": "light.desk"}, "service_data": {"brightness_pct": 70}})
        );
        assert_eq!(
            v(call(6, "switch.fan", "toggle", None)),
            json!({"id": 6, "type": "call_service", "domain": "switch", "service": "toggle",
                   "target": {"entity_id": "switch.fan"}})
        );
        assert_eq!(
            v(command(2, "config/area_registry/list")),
            json!({"id": 2, "type": "config/area_registry/list"})
        );
    }

    #[test]
    fn the_websocket_lives_under_the_address() {
        assert_eq!(
            websocket_url("http://192.168.1.69:8123/"),
            "ws://192.168.1.69:8123/api/websocket"
        );
        assert_eq!(
            websocket_url("http://ha.local:8123"),
            "ws://ha.local:8123/api/websocket"
        );
    }
}
