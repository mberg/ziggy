//! Keeps a WebSocket to Home Assistant open: subscribes to the configured entities, reads area
//! names once, sends calls, and reconnects after drops. `--fake` uses a pretend house instead.
//! Nothing is kept on disk.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;

use super::client::{self, Frame};
use super::types::{Control, Domain, EntityState, Page, States, area_names, brightness_of};
use crate::app::{HomeRequest, HomeUpdate, Input};

const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MOST: Duration = Duration::from_secs(60);
/// Pinged this often; a ping unanswered by the next one means the link is dead.
const PING_EVERY: Duration = Duration::from_secs(30);
/// Connecting and signing in must be done within this.
const SIGN_IN_WITHIN: Duration = Duration::from_secs(10);

const SUBSCRIPTION: u64 = 1;
/// Read once per connection, for area names.
const REGISTRIES: [(u64, &str); 3] = [
    (2, "config/entity_registry/list_for_display"),
    (3, "config/device_registry/list"),
    (4, "config/area_registry/list"),
];
/// Calls and pings are numbered from here.
const FIRST_CALL: u64 = REGISTRIES[REGISTRIES.len() - 1].0 + 1;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

pub enum Backend {
    Real {
        url: String,
        token: String,
    },
    /// `--fake`: a pretend house, see `fake_pages`.
    Fake,
}

pub fn next_retry(d: Duration) -> Duration {
    (d * 2).min(RETRY_MOST)
}

pub fn spawn_home(
    backend: Backend,
    entity_ids: Vec<String>,
    inputs: UnboundedSender<Input>,
) -> UnboundedSender<HomeRequest> {
    let (tx, rx) = mpsc::unbounded_channel();
    match backend {
        Backend::Real { url, token } => {
            tokio::spawn(run(
                client::websocket_url(&url),
                token,
                entity_ids,
                inputs,
                rx,
            ));
        }
        Backend::Fake => {
            tokio::spawn(run_fake(inputs, rx));
        }
    }
    tx
}

fn send(inputs: &UnboundedSender<Input>, update: HomeUpdate) {
    let _ = inputs.send(Input::Home(update));
}

enum End {
    RequestsClosed,
    Refused,
}

async fn run(
    url: String,
    token: String,
    ids: Vec<String>,
    inputs: UnboundedSender<Input>,
    mut requests: UnboundedReceiver<HomeRequest>,
) {
    let mut retry = RETRY_FIRST;
    loop {
        let started = Instant::now();
        match session(&url, &token, &ids, &inputs, &mut requests).await {
            Ok(End::RequestsClosed) => return,
            Ok(End::Refused) => {
                tracing::warn!("Home Assistant refused the token");
                send(&inputs, HomeUpdate::Refused);
                // A new token means `ziggy config`, which restarts the player.
                while let Some(request) = requests.recv().await {
                    refuse(&inputs, request);
                }
                return;
            }
            Err(e) => {
                tracing::warn!("Home Assistant: {e:#}");
                send(&inputs, HomeUpdate::Disconnected);
            }
        }
        if started.elapsed() > RETRY_MOST {
            retry = RETRY_FIRST;
        }
        // Wait out the delay, or less when asked to try now.
        let wake = tokio::time::sleep(retry);
        tokio::pin!(wake);
        loop {
            tokio::select! {
                _ = &mut wake => break,
                request = requests.recv() => match request {
                    None => return,
                    Some(HomeRequest::Reconnect) => break,
                    Some(other) => refuse(&inputs, other),
                },
            }
        }
        retry = next_retry(retry);
    }
}

fn refuse(inputs: &UnboundedSender<Input>, request: HomeRequest) {
    if let HomeRequest::Call { entity_id, .. } = request {
        send(
            inputs,
            HomeUpdate::CallFailed {
                entity_id,
                message: "Not connected".into(),
            },
        );
    }
}

/// Connects and signs in. `None` when Home Assistant refused the token.
async fn sign_in(url: &str, token: &str) -> anyhow::Result<Option<Socket>> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await?;
    loop {
        let Some(message) = ws.next().await else {
            anyhow::bail!("closed while signing in");
        };
        let Message::Text(text) = message? else {
            continue;
        };
        match client::parse(&text) {
            Frame::AuthRequired => ws.send(Message::text(client::auth(token))).await?,
            Frame::AuthOk => return Ok(Some(ws)),
            Frame::AuthInvalid(_) => return Ok(None),
            _ => {}
        }
    }
}

async fn session(
    url: &str,
    token: &str,
    ids: &[String],
    inputs: &UnboundedSender<Input>,
    requests: &mut UnboundedReceiver<HomeRequest>,
) -> anyhow::Result<End> {
    let ws = match tokio::time::timeout(SIGN_IN_WITHIN, sign_in(url, token)).await {
        Err(_) => anyhow::bail!("no answer while signing in"),
        Ok(signed_in) => match signed_in? {
            Some(ws) => ws,
            None => return Ok(End::Refused),
        },
    };
    let (mut tx, mut rx) = ws.split();
    send(inputs, HomeUpdate::Connected);
    // With no entity IDs Home Assistant would send every entity in the house.
    if !ids.is_empty() {
        tx.send(Message::text(client::subscribe(SUBSCRIPTION, ids)))
            .await?;
    }
    for (id, kind) in REGISTRIES {
        tx.send(Message::text(client::command(id, kind))).await?;
    }
    let mut next_id = FIRST_CALL;
    let mut states = States::default();
    let mut first_event = true;
    let mut registries: HashMap<u64, Value> = HashMap::new();
    // Calls waiting for their result, by message ID.
    let mut calls: HashMap<u64, String> = HashMap::new();
    let mut ping = tokio::time::interval(PING_EVERY);
    let mut awaiting_pong = false;
    loop {
        tokio::select! {
            message = rx.next() => {
                let Some(message) = message else {
                    anyhow::bail!("Home Assistant closed the connection")
                };
                let Message::Text(text) = message? else { continue };
                match client::parse(&text) {
                    Frame::Event { id: SUBSCRIPTION, event } => {
                        let (changed, mut removed) = states.apply(&event);
                        if first_event {
                            // Deleted or renamed in Home Assistant: no state is coming.
                            first_event = false;
                            removed.extend(ids.iter().filter(|id| states.get(id).is_none()).cloned());
                        }
                        if !changed.is_empty() {
                            send(inputs, HomeUpdate::States(changed));
                        }
                        if !removed.is_empty() {
                            send(inputs, HomeUpdate::Removed(removed));
                        }
                    }
                    Frame::Result { id: SUBSCRIPTION, ok, error, .. } => {
                        // No states would ever come: show the banner and try again later.
                        anyhow::ensure!(ok, "Home Assistant refused the subscription: {error}");
                    }
                    Frame::Result { id, ok, result, error }
                        if REGISTRIES.iter().any(|(r, _)| *r == id) =>
                    {
                        if !ok {
                            tracing::info!("Home Assistant didn't list {id}: {error}");
                        }
                        registries.insert(id, if ok { result } else { Value::Null });
                        if registries.len() == REGISTRIES.len() {
                            let [entities, devices, areas] = REGISTRIES.map(|(r, _)| &registries[&r]);
                            let mut areas = area_names(entities, devices, areas);
                            areas.retain(|id, _| ids.contains(id));
                            send(inputs, HomeUpdate::Areas(areas));
                        }
                    }
                    Frame::Result { id, ok, error, .. } => {
                        if let Some(entity_id) = calls.remove(&id)
                            && !ok
                        {
                            send(inputs, HomeUpdate::CallFailed { entity_id, message: error });
                        }
                    }
                    Frame::Pong => awaiting_pong = false,
                    _ => {}
                }
            }
            request = requests.recv() => match request {
                None => return Ok(End::RequestsClosed),
                Some(HomeRequest::Reconnect) => {
                    // The network changed: find out now whether this link survived.
                    tx.send(Message::text(client::ping(next_id))).await?;
                    next_id += 1;
                    awaiting_pong = true;
                    ping.reset();
                }
                Some(HomeRequest::Call { entity_id, service, brightness_pct }) => {
                    let id = next_id;
                    next_id += 1;
                    tx.send(Message::text(client::call(id, &entity_id, service, brightness_pct)))
                        .await?;
                    calls.insert(id, entity_id);
                }
            },
            _ = ping.tick() => {
                anyhow::ensure!(!awaiting_pong, "Home Assistant stopped answering");
                awaiting_pong = true;
                tx.send(Message::text(client::ping(next_id))).await?;
                next_id += 1;
            }
        }
    }
}

/// The pretend house `--fake` always shows, whatever the config's Home Assistant pages.
pub fn fake_pages() -> Vec<Page> {
    let c = |id: &str| Control {
        entity_id: id.into(),
        domain: Domain::of(id).unwrap(),
        name: None,
    };
    vec![
        Page {
            name: "Bedroom".into(),
            tiles: [
                "light.bedroom_ceiling",
                "light.bedroom_desk",
                "light.bedroom_night",
                "switch.fairy_lights",
                "fan.bedroom",
            ]
            .map(c)
            .to_vec(),
            scenes: vec![c("scene.bedtime"), c("script.reading")],
        },
        Page {
            name: "Downstairs".into(),
            tiles: [
                "light.kitchen",
                "light.sofa_lamp",
                "switch.tv",
                "light.porch",
            ]
            .map(c)
            .to_vec(),
            scenes: vec![c("scene.movie")],
        },
    ]
}

async fn run_fake(inputs: UnboundedSender<Input>, mut requests: UnboundedReceiver<HomeRequest>) {
    let devices = [
        (
            "light.bedroom_ceiling",
            "Ceiling light",
            "Bedroom",
            "on",
            Some(178),
        ),
        ("light.bedroom_desk", "Desk lamp", "Bedroom", "off", None),
        (
            "light.bedroom_night",
            "Night light",
            "Bedroom",
            "on",
            Some(38),
        ),
        ("switch.fairy_lights", "Fairy lights", "Bedroom", "on", None),
        ("fan.bedroom", "Fan", "Bedroom", "off", None),
        (
            "light.kitchen",
            "Kitchen lights",
            "Kitchen",
            "on",
            Some(230),
        ),
        (
            "light.sofa_lamp",
            "Sofa lamp",
            "Living room",
            "on",
            Some(100),
        ),
        ("switch.tv", "TV plug", "Living room", "off", None),
        ("light.porch", "Porch light", "Outside", "off", None),
    ];
    let mut house: HashMap<String, EntityState> = devices
        .iter()
        .map(|(id, name, _, state, b)| {
            (
                id.to_string(),
                EntityState {
                    state: state.to_string(),
                    name: Some(name.to_string()),
                    brightness: *b,
                    // The ceiling light is a group, so `--fake` shows one.
                    members: if *id == "light.bedroom_ceiling" { 2 } else { 0 },
                },
            )
        })
        .collect();
    let areas = devices
        .iter()
        .map(|(id, _, area, _, _)| (id.to_string(), area.to_string()))
        .collect();
    send(&inputs, HomeUpdate::Connected);
    send(&inputs, HomeUpdate::Areas(areas));
    send(
        &inputs,
        HomeUpdate::States(house.clone().into_iter().collect()),
    );
    while let Some(request) = requests.recv().await {
        let HomeRequest::Call {
            entity_id,
            service,
            brightness_pct,
        } = request
        else {
            continue;
        };
        let Some(state) = house.get_mut(&entity_id) else {
            if Domain::of(&entity_id).is_some_and(|d| !d.is_tile()) {
                continue; // Scenes and scripts just run.
            }
            send(
                &inputs,
                HomeUpdate::CallFailed {
                    entity_id,
                    message: "Entity not found".into(),
                },
            );
            continue;
        };
        let on = match service {
            "turn_on" => true,
            "turn_off" => false,
            _ => !state.is_on(),
        };
        state.state = if on { "on" } else { "off" }.into();
        // Only a lit light has a brightness.
        let light = Domain::of(&entity_id) == Some(Domain::Light);
        state.brightness = match (on && light, brightness_pct) {
            (false, _) => None,
            (true, Some(pct)) => Some(brightness_of(pct)),
            (true, None) => state.brightness.or(Some(255)),
        };
        send(
            &inputs,
            HomeUpdate::States(vec![(entity_id, state.clone())]),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::WebSocketStream;

    /// Waits for the next update the service sends, failing instead of hanging.
    async fn next(rx: &mut UnboundedReceiver<Input>) -> HomeUpdate {
        match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
            Ok(Some(Input::Home(update))) => update,
            Ok(other) => panic!("expected a Home update, got {other:?}"),
            Err(_) => panic!("no Home update within 5s"),
        }
    }

    /// Skips updates until one `pick` takes.
    async fn wait_for<T>(
        rx: &mut UnboundedReceiver<Input>,
        mut pick: impl FnMut(HomeUpdate) -> Option<T>,
    ) -> T {
        loop {
            if let Some(found) = pick(next(rx).await) {
                return found;
            }
        }
    }

    /// A pretend Home Assistant on a free local port, and the address to give `spawn_home`.
    async fn listen() -> (TcpListener, String) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (listener, url)
    }

    async fn accept(listener: &TcpListener) -> WebSocketStream<TcpStream> {
        let (stream, _) = listener.accept().await.unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    }

    async fn say(ws: &mut WebSocketStream<TcpStream>, v: Value) {
        ws.send(Message::text(v.to_string())).await.unwrap();
    }

    async fn hear(ws: &mut WebSocketStream<TcpStream>) -> Value {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                _ => continue,
            }
        }
    }

    /// Asks for the token and accepts it.
    async fn sign_in(ws: &mut WebSocketStream<TcpStream>) {
        say(ws, json!({"type": "auth_required"})).await;
        assert_eq!(hear(ws).await["access_token"], "tok");
        say(ws, json!({"type": "auth_ok"})).await;
    }

    fn real(url: &str, ids: &[&str]) -> (UnboundedSender<HomeRequest>, UnboundedReceiver<Input>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let backend = Backend::Real {
            url: url.into(),
            token: "tok".into(),
        };
        let ids = ids.iter().map(|id| id.to_string()).collect();
        (spawn_home(backend, ids, tx), rx)
    }

    #[test]
    fn retries_back_off_to_a_minute() {
        let mut d = RETRY_FIRST;
        let mut seen = vec![d.as_secs()];
        for _ in 0..7 {
            d = next_retry(d);
            seen.push(d.as_secs());
        }
        assert_eq!(seen, vec![2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[tokio::test]
    async fn a_refused_token_is_reported_and_calls_fail() {
        let (listener, url) = listen().await;
        let (requests, mut rx) = real(&url, &["light.desk"]);
        let mut ws = accept(&listener).await;
        say(&mut ws, json!({"type": "auth_required"})).await;
        hear(&mut ws).await;
        say(
            &mut ws,
            json!({"type": "auth_invalid", "message": "Invalid access token"}),
        )
        .await;
        assert_eq!(next(&mut rx).await, HomeUpdate::Refused);
        requests
            .send(HomeRequest::Call {
                entity_id: "light.desk".into(),
                service: "toggle",
                brightness_pct: None,
            })
            .unwrap();
        assert_eq!(
            next(&mut rx).await,
            HomeUpdate::CallFailed {
                entity_id: "light.desk".into(),
                message: "Not connected".into()
            }
        );
    }

    #[tokio::test]
    async fn a_session_reports_states_missing_entities_and_areas() {
        let (listener, url) = listen().await;
        let (_requests, mut rx) = real(&url, &["light.desk", "light.gone"]);
        let mut ws = accept(&listener).await;
        sign_in(&mut ws).await;
        assert_eq!(next(&mut rx).await, HomeUpdate::Connected);
        let subscribe = hear(&mut ws).await;
        assert_eq!(subscribe["type"], "subscribe_entities");
        assert_eq!(subscribe["entity_ids"], json!(["light.desk", "light.gone"]));
        for _ in REGISTRIES {
            hear(&mut ws).await;
        }
        say(
            &mut ws,
            json!({"id": 1, "type": "result", "success": true, "result": null}),
        )
        .await;
        say(
            &mut ws,
            json!({"id": 1, "type": "event", "event": {"a": {
                "light.desk": {"s": "on", "a": {"friendly_name": "Desk lamp", "brightness": 255}}
            }}}),
        )
        .await;
        let states = wait_for(&mut rx, |u| match u {
            HomeUpdate::States(s) => Some(s),
            _ => None,
        })
        .await;
        assert_eq!(states[0].0, "light.desk");
        assert_eq!(
            next(&mut rx).await,
            HomeUpdate::Removed(vec!["light.gone".into()])
        );
        let entities = json!({"entities": [
            {"ei": "light.desk", "ai": "study"},
            {"ei": "light.elsewhere", "ai": "study"}
        ]});
        let areas = json!([{"area_id": "study", "name": "Study"}]);
        for (id, result) in [(2, entities), (3, json!([])), (4, areas)] {
            say(
                &mut ws,
                json!({"id": id, "type": "result", "success": true, "result": result}),
            )
            .await;
        }
        let areas = wait_for(&mut rx, |u| match u {
            HomeUpdate::Areas(a) => Some(a),
            _ => None,
        })
        .await;
        assert_eq!(
            areas,
            HashMap::from([("light.desk".to_string(), "Study".to_string())])
        );
    }

    #[tokio::test]
    async fn a_failed_call_reports_home_assistants_message() {
        let (listener, url) = listen().await;
        let (requests, mut rx) = real(&url, &["light.desk"]);
        let mut ws = accept(&listener).await;
        sign_in(&mut ws).await;
        assert_eq!(next(&mut rx).await, HomeUpdate::Connected);
        requests
            .send(HomeRequest::Call {
                entity_id: "light.desk".into(),
                service: "turn_on",
                brightness_pct: Some(40),
            })
            .unwrap();
        let call = loop {
            let v = hear(&mut ws).await;
            if v["type"] == "call_service" {
                break v;
            }
        };
        assert!(call["id"].as_u64().unwrap() >= FIRST_CALL);
        say(
            &mut ws,
            json!({"id": call["id"], "type": "result", "success": false,
                   "error": {"code": "home_assistant_error", "message": "Light is busy"}}),
        )
        .await;
        let failed = wait_for(&mut rx, |u| match u {
            HomeUpdate::CallFailed { .. } => Some(u),
            _ => None,
        })
        .await;
        assert_eq!(
            failed,
            HomeUpdate::CallFailed {
                entity_id: "light.desk".into(),
                message: "Light is busy".into()
            }
        );
    }

    #[tokio::test]
    async fn a_refused_subscription_disconnects_and_tries_again() {
        let (listener, url) = listen().await;
        let (requests, mut rx) = real(&url, &["light.desk"]);
        let mut ws = accept(&listener).await;
        sign_in(&mut ws).await;
        assert_eq!(next(&mut rx).await, HomeUpdate::Connected);
        assert_eq!(hear(&mut ws).await["type"], "subscribe_entities");
        say(
            &mut ws,
            json!({"id": 1, "type": "result", "success": false,
                   "error": {"code": "unknown_command", "message": "Unknown command."}}),
        )
        .await;
        assert_eq!(next(&mut rx).await, HomeUpdate::Disconnected);
        // Asked to try now rather than wait out the delay: it comes back and subscribes again.
        requests.send(HomeRequest::Reconnect).unwrap();
        let mut ws = accept(&listener).await;
        sign_in(&mut ws).await;
        assert_eq!(next(&mut rx).await, HomeUpdate::Connected);
        assert_eq!(hear(&mut ws).await["type"], "subscribe_entities");
    }

    #[tokio::test]
    async fn a_closed_connection_is_reported() {
        let (listener, url) = listen().await;
        let (_requests, mut rx) = real(&url, &["light.desk"]);
        let mut ws = accept(&listener).await;
        sign_in(&mut ws).await;
        assert_eq!(next(&mut rx).await, HomeUpdate::Connected);
        ws.close(None).await.unwrap();
        drop(ws);
        assert_eq!(next(&mut rx).await, HomeUpdate::Disconnected);
    }

    #[tokio::test]
    async fn the_fake_answers_taps_with_new_states() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let requests = spawn_home(Backend::Fake, Vec::new(), tx);
        wait_for(&mut rx, |u| (u == HomeUpdate::Connected).then_some(())).await;
        requests
            .send(HomeRequest::Call {
                entity_id: "light.bedroom_desk".into(),
                service: "turn_on",
                brightness_pct: Some(40),
            })
            .unwrap();
        let desk = wait_for(&mut rx, |u| match u {
            HomeUpdate::States(states) => states
                .into_iter()
                .find(|(id, s)| id == "light.bedroom_desk" && s.is_on()),
            _ => None,
        })
        .await;
        assert_eq!(desk.1.percent(), Some(40));
    }

    #[tokio::test]
    async fn the_fake_house_has_every_fake_tile() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _requests = spawn_home(Backend::Fake, Vec::new(), tx);
        let mut have: Vec<String> = wait_for(&mut rx, |u| match u {
            HomeUpdate::States(states) => Some(states.into_iter().map(|(id, _)| id).collect()),
            _ => None,
        })
        .await;
        let mut want: Vec<String> = fake_pages()
            .into_iter()
            .flat_map(|p| p.tiles)
            .map(|c| c.entity_id)
            .collect();
        have.sort();
        want.sort();
        assert_eq!(have, want);
    }

    #[tokio::test]
    async fn only_lights_in_the_fake_get_a_brightness() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let requests = spawn_home(Backend::Fake, Vec::new(), tx);
        wait_for(&mut rx, |u| (u == HomeUpdate::Connected).then_some(())).await;
        requests
            .send(HomeRequest::Call {
                entity_id: "fan.bedroom".into(),
                service: "turn_on",
                brightness_pct: Some(40),
            })
            .unwrap();
        let fan = wait_for(&mut rx, |u| match u {
            HomeUpdate::States(states) => states
                .into_iter()
                .find(|(id, s)| id == "fan.bedroom" && s.is_on()),
            _ => None,
        })
        .await;
        assert_eq!(fan.1.brightness, None);
    }
}
