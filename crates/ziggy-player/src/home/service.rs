//! Keeps a WebSocket to Home Assistant open: subscribes to the configured entities, reads area
//! names once, sends calls, and reconnects after drops. `--fake` uses a pretend house instead.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tungstenite::tungstenite::Message;

use super::client::{self, Frame};
use super::types::{Control, Domain, EntityState, Page, States, area_names, brightness_of};
use crate::app::{HomeRequest, HomeUpdate, Input};
use crate::library::cache::DiskCache;

pub const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MOST: Duration = Duration::from_secs(60);
/// A connection that answers no ping for this long is dead.
const PING_EVERY: Duration = Duration::from_secs(30);
const STATES_KEY: &str = "home-states";
const AREAS_KEY: &str = "home-areas";

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
    cache: Arc<DiskCache>,
    inputs: UnboundedSender<Input>,
) -> UnboundedSender<HomeRequest> {
    let (tx, rx) = mpsc::unbounded_channel();
    match backend {
        Backend::Real { url, token } => {
            tokio::spawn(run(
                client::websocket_url(&url),
                token,
                entity_ids,
                cache,
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
    cache: Arc<DiskCache>,
    inputs: UnboundedSender<Input>,
    mut requests: UnboundedReceiver<HomeRequest>,
) {
    // Names and states from last time, so tiles have names before Home Assistant answers.
    if let Some(states) = cache.read::<Vec<(String, EntityState)>>(STATES_KEY) {
        send(&inputs, HomeUpdate::States(states));
    }
    if let Some(areas) = cache.read::<HashMap<String, String>>(AREAS_KEY) {
        send(&inputs, HomeUpdate::Areas(areas));
    }
    let mut retry = RETRY_FIRST;
    loop {
        let started = Instant::now();
        match session(&url, &token, &ids, &cache, &inputs, &mut requests).await {
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
                message: "not connected".into(),
            },
        );
    }
}

async fn session(
    url: &str,
    token: &str,
    ids: &[String],
    cache: &DiskCache,
    inputs: &UnboundedSender<Input>,
    requests: &mut UnboundedReceiver<HomeRequest>,
) -> anyhow::Result<End> {
    let (ws, _) = tokio_tungstenite::connect_async(url).await?;
    let (mut tx, mut rx) = ws.split();
    loop {
        let Some(message) = rx.next().await else {
            anyhow::bail!("closed while signing in");
        };
        let Message::Text(text) = message? else {
            continue;
        };
        match client::parse(&text) {
            Frame::AuthRequired => tx.send(Message::text(client::auth(token))).await?,
            Frame::AuthOk => break,
            Frame::AuthInvalid(_) => return Ok(End::Refused),
            _ => {}
        }
    }
    send(inputs, HomeUpdate::Connected);
    const SUBSCRIPTION: u64 = 1;
    const REGISTRIES: [(u64, &str); 3] = [
        (2, "config/entity_registry/list_for_display"),
        (3, "config/device_registry/list"),
        (4, "config/area_registry/list"),
    ];
    // With no entity IDs Home Assistant would send every entity in the house.
    if !ids.is_empty() {
        tx.send(Message::text(client::subscribe(SUBSCRIPTION, ids)))
            .await?;
    }
    for (id, kind) in REGISTRIES {
        tx.send(Message::text(client::command(id, kind))).await?;
    }
    let mut next_id = 5u64;
    let mut states = States::default();
    let mut registries: HashMap<u64, Value> = HashMap::new();
    // Calls waiting for their result, by message ID. The subscription's own result (ID 1) finds
    // nothing here and is ignored.
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
                        let (changed, removed) = states.apply(&event);
                        if !changed.is_empty() {
                            send(inputs, HomeUpdate::States(changed));
                        }
                        if !removed.is_empty() {
                            send(inputs, HomeUpdate::Removed(removed));
                        }
                        let _ = cache.write(STATES_KEY, &states.snapshot());
                    }
                    Frame::Result { id, ok, result, error } if (2..=4).contains(&id) => {
                        if !ok {
                            tracing::info!("Home Assistant didn't list {id}: {error}");
                        }
                        registries.insert(id, if ok { result } else { Value::Null });
                        if registries.len() == REGISTRIES.len() {
                            let areas =
                                area_names(&registries[&2], &registries[&3], &registries[&4]);
                            let _ = cache.write(AREAS_KEY, &areas);
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
                Some(HomeRequest::Reconnect) => {}
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
                let id = next_id;
                next_id += 1;
                tx.send(Message::text(client::ping(id))).await?;
            }
        }
    }
}

/// The pretend house `--fake` shows when the config has no Home Assistant pages.
pub fn fake_pages() -> Vec<Page> {
    let c = |id: &str| Control {
        entity_id: id.into(),
        domain: Domain::of(id).unwrap(),
        name: None,
    };
    vec![
        Page {
            name: "Micah's room".into(),
            tiles: [
                "light.micah_ceiling",
                "light.micah_desk",
                "light.micah_night",
                "switch.fairy_lights",
                "fan.micah",
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
    let names = [
        (
            "light.micah_ceiling",
            "Ceiling light",
            "Micah's room",
            "on",
            Some(178),
        ),
        ("light.micah_desk", "Desk lamp", "Micah's room", "off", None),
        (
            "light.micah_night",
            "Night light",
            "Micah's room",
            "on",
            Some(38),
        ),
        (
            "switch.fairy_lights",
            "Fairy lights",
            "Micah's room",
            "on",
            None,
        ),
        ("fan.micah", "Fan", "Micah's room", "off", None),
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
    let mut house: HashMap<String, EntityState> = names
        .iter()
        .map(|(id, name, _, state, b)| {
            (
                id.to_string(),
                EntityState {
                    state: state.to_string(),
                    name: Some(name.to_string()),
                    brightness: *b,
                },
            )
        })
        .collect();
    let areas = names
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
        state.brightness = match (on, brightness_pct) {
            (false, _) => None,
            (true, Some(pct)) => Some(brightness_of(pct)),
            (true, None) if entity_id.starts_with("light.") => state.brightness.or(Some(255)),
            (true, None) => None,
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
    async fn the_fake_answers_taps_with_new_states() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(DiskCache::new(dir.path().to_path_buf()).unwrap());
        let requests = spawn_home(Backend::Fake, Vec::new(), cache, tx);
        let mut connected = false;
        while let Some(Input::Home(update)) = rx.recv().await {
            if update == HomeUpdate::Connected {
                connected = true;
                break;
            }
        }
        assert!(connected);
        requests
            .send(HomeRequest::Call {
                entity_id: "light.micah_desk".into(),
                service: "turn_on",
                brightness_pct: Some(40),
            })
            .unwrap();
        loop {
            if let Some(Input::Home(HomeUpdate::States(states))) = rx.recv().await
                && let Some((_, s)) = states.iter().find(|(id, _)| id == "light.micah_desk")
                && s.is_on()
            {
                assert_eq!(s.percent(), Some(40));
                break;
            }
        }
    }

    #[tokio::test]
    async fn the_fake_house_has_every_fake_tile() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(DiskCache::new(dir.path().to_path_buf()).unwrap());
        let _requests = spawn_home(Backend::Fake, Vec::new(), cache, tx);
        let mut have: Vec<String> = loop {
            if let Some(Input::Home(HomeUpdate::States(states))) = rx.recv().await {
                break states.into_iter().map(|(id, _)| id).collect();
            }
        };
        let mut want: Vec<String> = fake_pages()
            .into_iter()
            .flat_map(|p| p.tiles)
            .map(|c| c.entity_id)
            .collect();
        have.sort();
        want.sort();
        assert_eq!(have, want);
    }
}
