# Home Assistant controls — Design

Date: 2026-10-31
Status: Draft for review
Mockup: https://claude.ai/artifact/FNYyVRpWjatnuZZY4pRqP5

## Purpose

Let a Ziggy player switch lights, plugs and scenes in the house through the Home Assistant
instance on the local network. Each player is set up on its own: one may show its room's lights,
another may show nothing.

Success means:

- A player with Home Assistant configured shows a Home screen; one without it looks as it does today.
- A light can be switched on or off with one tap, and dimmed with a large slider.
- A change made elsewhere (the Home Assistant app, a wall switch) shows on the screen within a second.
- When Home Assistant can't be reached the screen says so and recovers by itself.

## Decisions

| Topic | Decision |
|---|---|
| What each player shows | Listed in its `/etc/ziggy/config.toml`, as named pages of entity IDs |
| Home Assistant dashboards | Not used. Ziggy draws its own tiles; Home Assistant's web dashboards can't run on the Pi |
| Connection | Home Assistant's WebSocket API, on the `tokio-tungstenite` Ziggy already uses for Soloist |
| Sign-in | A long-lived access token of a non-admin "Ziggy" user in Home Assistant, one file per player |
| Controls in v1 | Lights (on/off, brightness), switches and plugs, fans (on/off), scenes, scripts |
| Voice | Later; not in v1 |

Found on the network on 2026-10-31: Home Assistant 2026.2.3 at `http://192.168.1.69:8123`
(mDNS `_home-assistant._tcp`, name "Home"), reachable from both players.

Rust crates were considered: `hass-rs` 0.5 (async WebSocket client, little used) and
`homeassistant` 0.3 (unmaintained since 2020). The messages Ziggy needs are few, so a small client
of our own is simpler than adapting either.

## Configuration

Home is on when the config has a `[home_assistant]` section.

```toml
[home_assistant]
url = "http://192.168.1.69:8123"
token_file = "/var/lib/ziggy/ha-token"   # default: <state_dir>/ha-token

[[home_assistant.page]]
name = "Micah's room"
controls = ["light.micah_ceiling", "light.micah_desk", "light.micah_night",
            "switch.fairy_lights", "fan.micah", "scene.bedtime", "script.reading"]

[[home_assistant.page]]
name = "Everyday"
controls = ["light.porch", "light.hall", "light.kitchen",
            { entity = "light.micah_ceiling", name = "Micah's ceiling" },
            "scene.good_night"]
```

- A control is an entity ID, or a table with `entity` and an optional `name` that replaces Home
  Assistant's name on the tile.
- Lights, switches and fans become tiles, in the order listed. Scenes and scripts become buttons
  along the bottom of the page.
- An entity of any other domain is a config error, reported at load like other config errors.
- `ziggy config` asks for the URL (offering the instance it finds over mDNS) and the token, and
  copies the token to the player like the other sign-ins. Pages are written in the config file.

## Screen

The mockup shows each of these at 800×480 in the Midnight scheme.

- **Rail**: a "Home" item, second, under Search. It shows only when Home is configured, as Books
  does for Audiobookshelf.
- **Page chips** along the top, one per configured page, scrolling sideways when they don't fit.
  The first page opens by default; the last one used is remembered while the player runs.
- **Tiles** in three columns, 100 px tall, scrolling down when a page has more than six.
  - Name from Home Assistant (or the config) and an icon by domain.
  - On a page whose tiles come from more than one Home Assistant area, the area name shows under
    the tile's name.
  - On: accent fill. Off: surface colour.
  - **Lights are split.** The right third is a strip that shows "Off", or ☀ and the brightness
    ("70%"). It is there in both states so the name never moves.
    - Tapping a light that's off, anywhere on it, turns it on.
    - Tapping the left part of a light that's on turns it off.
    - Tapping the strip of a light that's on selects it (outline) and opens the brightness bar.
  - Switches and fans toggle on tap.
- **Scene buttons** in a row at the bottom of the page. A tap runs the scene or script and outlines
  the button until a tile changes.
- **Brightness bar**: while a light is selected, the music bar at the bottom becomes:
  the light's name and level, a full-width slider (about 430 × 60 px, drag anywhere on it), an
  On/Off button and × back to the music. Dragging a light that's off turns it on. After 8 seconds
  without a touch, or on leaving the Home screen, the bar goes back to the music.
- **Unreachable**: a banner, "Can't reach Home Assistant. Trying again…", over greyed tiles that
  don't respond. A refused token says "Home Assistant refused Ziggy's sign-in. Run `ziggy config`."
- **Missing entity**: a greyed tile reading "Not in Home Assistant". An entity Home Assistant
  reports as `unavailable` reads "Unavailable".

## Architecture

A new `home` module, laid out like `audiobooks`:

```
home/
  types.rs    Control, Tile (kind, name, area, on, level), Page — plain data, no I/O
  client.rs   WebSocket: auth, subscribe, call_service; parses Home Assistant's messages
  service.rs  keeps the connection up, turns messages into Input::Home(update),
              takes HomeRequest from the app core
```

- **Connecting**: `auth` with the token, then:
  - `subscribe_entities` with the configured entity IDs, which sends their states now and every
    change after;
  - `config/entity_registry/list_for_display` and `config/area_registry/list` once, for each
    entity's area name. An entity's own area wins; otherwise its device's area.
- **Commands**: `call_service`:
  - `light.turn_on` (with `brightness_pct` when dimming), `light.turn_off`;
  - `switch.toggle`, `fan.toggle`;
  - `scene.turn_on`, `script.turn_on`.
- **Optimistic taps**: the app core shows a tap's result at once. The state Home Assistant then
  reports wins. A failed call puts the tile back and shows the error in the banner for a few
  seconds.
- **Dragging**: while the slider moves, brightness is sent at most every 250 ms, and once more
  when the finger lifts.
- **Reconnecting**: on a drop, retry after 2 s, doubling to 60 s. Retry at once when the Wi-Fi
  comes back, as the library does (`Input` from the Wi-Fi watch).
- **App core**: `HomeState` (pages, tiles by entity, selected light, banner, bar timer) in
  `app/state.rs`. `UiAction::Home*` for tile, strip, scene, chip, slider and bar taps. Effects
  become `HomeRequest`s. The core holds the rules above (which tap does what, the 8 s timer), so
  they are unit-tested without a network.
- **UI**: `screens/home.slint` with a `HomeTile` component, plus a brightness variant of the mini
  player. `Section::Home` is added to the rail.
- **`--fake`**: a fake Home Assistant with a few rooms, so the screen can be tried on the Mac.

## Testing

- **Config**: pages, controls as strings and tables, the default token path, an unknown domain
  as an error, and no `[home_assistant]` meaning Home is off.
- **Client parsing**, from recorded Home Assistant messages: `auth_ok`, `auth_invalid`,
  `subscribe_entities` initial and change events, registry lists to area names, and service-call
  errors.
- **App core**:
  - a tap on an off light sends `turn_on`;
  - the left part of an on light sends `turn_off`;
  - the strip selects and opens the bar;
  - slider throttling;
  - the bar returning after 8 s;
  - optimistic state and revert on failure;
  - offline and missing-entity tiles.
- **Manual**, added to `docs/testing.md`: against the real instance on the Mac mini, from a
  player, including turning the Mac mini's Home Assistant off and on.

## Not in v1

- Voice requests ("Ziggy, turn off the lamp").
- Other domains: climate, covers, media players, locks.
- Editing pages on the player.
- Reading Home Assistant dashboards or labels.
