# Testing Ziggy

Manual checks for developers, on the Mac and then on a Pi. To set a player up the normal way, see the [README](../README.md).

The Spotify sign-in commands below (`auth`, `auth-web`, `probe`) are hidden commands of the `ziggy` tool; `ziggy setup` runs them for you.

## On the Mac

### 0. Prerequisites

- Xcode command line tools: `xcode-select --install`
- Rust 1.97.1. The repo has a `.tool-versions` file, so with asdf run `asdf install`. Without asdf, use `rustup toolchain install 1.97.1`.
  - If `cargo` is still "command not found" after `asdf install`, the asdf shims are not on your PATH. Add `export PATH="$HOME/.asdf/shims:$PATH"` to `~/.zshrc` and open a new terminal.
- A Spotify Premium account to test with. It can be your own or another member of your Family plan.
- Docker Desktop, needed only to build for the Pi.

Get the code:

```bash
git clone https://github.com/mberg/ziggy.git
cd ziggy
cargo test --workspace       # expect 82 passed
```

The first build takes a few minutes. Later builds are quick.

### 1. UI with fake music (no Spotify needed)

```bash
cargo run -p ziggy-player -- --fake
```

An 800×480 window opens with a made-up library. A mouse click is a tap, and dragging scrolls.

To check the idle screens quickly, first add these two lines to `dev/config.toml`, and remove them afterwards:

```toml
dim_after_secs = 10
off_after_secs = 20
```

Check:

- [ ] "Loading…" shows briefly, then the four album tiles: Albums is the home screen. Long names end in "…" (see Playlists).
- [ ] The left rail (Playlists, Albums, Liked, Recent) switches the content. Liked opens a track list directly.
- [ ] Tapping a tile opens its track list. Play opens Now Playing.
- [ ] Shuffle starts on the last song.
- [ ] The progress bar moves every second, and dragging it seeks.
- [ ] Next, previous and the volume slider work.
- [ ] Back returns to the track list.
- [ ] The mini player shows at the bottom, and tapping it reopens Now Playing.
- [ ] Shuffle and repeat highlight when on. The third tap on repeat shows a "1" badge.
- [ ] Idle, with playback paused:
  - after 10s, a clock appears;
  - after 20s, the screen goes black;
  - a tap wakes it.
- [ ] Nothing is clipped or off-center at 800×480.

To take a screenshot of the window:

```bash
screencapture -l$(osascript -e 'tell app "System Events" to id of window 1 of process "ziggy-player"') /tmp/ziggy.png
```

### 2. Sign in to Spotify

```bash
cargo run -p ziggy-setup --bin ziggy -- auth --state-dir secrets/dev
```

A browser opens. Sign in with the Spotify account you want to test with. This saves `secrets/dev/librespot/credentials.json`. The `secrets/` folder is git-ignored.

Then check that the Web API accepts the session's tokens:

```bash
cargo run -p ziggy-setup --bin ziggy -- probe --state-dir secrets/dev
```

- [ ] Each line shows `HTTP 200`. At least one of the two "playlist items" lines must be 200.
- [ ] The last line reads `All endpoints work with librespot tokens.`
- [ ] The first lines name the library and playback accounts, and they match. To check which account a device uses later, look at the small label at the bottom of the left rail.

**If probe fails** with a 403 from Spotify's token service ("Invalid request") or 401/403 on the endpoints, Spotify is refusing to give the librespot session a Web API token. Playback still works, but the library has to come through your own Spotify developer app instead. This is "Contingency A" in the plan:

1. Create an app at https://developer.spotify.com/dashboard:
   - Redirect URI: `http://127.0.0.1:8898/login`
   - Tick the "Web API" box.
   - Under "User Management", add the email of each Spotify account that will use the player or the probe below returns 403.
   One developer app serves every device and every person: you do not need one per device. Its users are capped in development mode, so check the limit on the dashboard.
2. Copy the app's Client ID, sign in again through the app, then probe again:
   ```bash
   cargo run -p ziggy-setup --bin ziggy -- auth-web --state-dir secrets/dev --client-id <CLIENT_ID>
   cargo run -p ziggy-setup --bin ziggy -- probe --state-dir secrets/dev
   ```
   This saves `secrets/dev/web-auth.json`. The probe uses it automatically and should end with `All endpoints work with developer-app tokens.`
3. In step 3 below, also copy `web-auth.json` into `dev/state/`. The player uses it when it is there.

For the Pi, run `auth-web` with `--state-dir secrets/<name>` as well. `scripts/deploy.sh` copies `web-auth.json` to the Pi when it exists.

### 3. Real playback on the Mac

```bash
mkdir -p dev/state/librespot
cp secrets/dev/librespot/credentials.json dev/state/librespot/
cp secrets/dev/web-auth.json dev/state/ 2>/dev/null   # only after Contingency A
cargo run -p ziggy-player
```

For more detailed logs, run `RUST_LOG=debug cargo run -p ziggy-player`.

Check:

- [ ] Your real playlists and albums show, with cover art. Recent and Liked fill in.
- [ ] Playing a playlist starts audio on the Mac within about 1 second. The title and artist are correct.
- [ ] Pause, next, previous, seek, volume, shuffle and repeat respond within about half a second.
- [ ] The Spotify app on your phone lists "Ziggy Dev" as the playing device.
- [ ] Liked: open it from the rail and press Play. Audio starts.
- [ ] In the Spotify app on your phone, move playback to the phone. Ziggy shows Paused. Press Play in Ziggy and it resumes the same song.
- [ ] Turn Wi-Fi off:
  - the grids stay visible;
  - Play shows "No internet right now".
- [ ] Turn Wi-Fi back on. The library refreshes and Play works again within about a minute.
- [ ] Sign-in failure:
  1. Quit the app.
  2. Move `dev/state/librespot/credentials.json` away.
  3. Start the app again. It shows "Spotify sign-in needed".
  4. Put the file back.

### 4. Search

Check, with the real account:

- [ ] Search in the rail opens the search screen with the keyboard up. The mini player hides while the keyboard is open.
- [ ] Typing shows matches from your own playlists, albums and liked songs at once, under "In your library".
- [ ] About half a second after you stop typing, "On Spotify" results appear: songs, artists, albums and playlists.
- [ ] The Mac keyboard types into the search box too. Return or Escape closes the keyboard; tapping the box opens it again.
- [ ] Tapping a song plays it, and its album carries on afterwards.
- [ ] Tapping an album or playlist opens its track list. Back returns to the results.
- [ ] Tapping an artist opens their albums. Play plays the artist.
- [ ] A playlist owned by someone else says Spotify won't list its songs, and Play still works.
- [ ] With Wi-Fi off, library matches still show, with a "No internet right now" note.

### 5. Playlists

Editing, liking and following artists need five more Spotify permissions. Sign in through the developer app once more, then copy the new file over:

```bash
cargo run -p ziggy-setup --bin ziggy -- auth-web --state-dir secrets/dev --client-id <CLIENT_ID>
cp secrets/dev/web-auth.json dev/state/
```

Check:

- [ ] The plus button on a track row, on Now Playing, and on a search song opens "Add to playlist" with only your own playlists.
- [ ] Picking one shows "Added to <name>". Adding the same song again shows "Already in <name>".
- [ ] "New playlist" opens a name field with the keyboard. Save creates it at the top of Playlists with the song in it.
- [ ] On Now Playing, the heart next to the plus fills in when the song is in Liked Songs. Tapping it likes or unlikes the song, and Liked Songs updates.
- [ ] Your own playlists show a pencil next to Back. Someone else's don't.
- [ ] In edit mode, the minus button removes a song, and dragging the handle on the left moves it.
- [ ] Rename changes the name everywhere. Delete asks first, then removes the playlist and returns to Playlists.
- [ ] The Spotify app on your phone shows each change.
- [ ] With Wi-Fi off, an edit shows "No internet right now" and nothing changes.

### Recent and play history

- [ ] Every song played on this player is logged in `dev/state/history.db` (SQLite), with how long it actually played.
- [ ] Recent → "Albums & playlists" shows where recent songs were played from, newest first; songs skipped within 30 seconds don't count.
- [ ] Recent → "Songs" lists recent songs with when they played ("12 min ago", "Yesterday"); tapping one plays it from where it was heard, with no Spotify search.
- [ ] A player with no history yet shows Spotify's recently-played list until it has some.

### Sleep timer

- [ ] On the playing-song screen, tapping the moon at the end of the volume row starts the sleep timer straight away: 30 minutes unless Settings says 60.
- [ ] The moon turns gold and shows the minutes left. About 3 seconds after the last touch the screen fades over 5 seconds to dim (no clock), then goes dark after a minute. A tap wakes it instantly.
- [ ] In the last 5 seconds the music fades out, then pauses; the next Play is at the old volume.
- [ ] Tapping the moon while it runs turns it off.
- [ ] Settings → Sleep timer switches between 30 and 60 minutes without restarting the player.
- [ ] Settings → Colours switches between Midnight (default), Ocean, Forest and Daylight at once, and the choice survives a restart.

### Sonos (at home, with Sonos speakers on the same network)

- [ ] Settings → Speaker → Find speakers lists your Sonos rooms (with `--fake`: Kitchen, Living Room, Office).
- [ ] Tapping a room restarts the player; Settings shows "Sonos: <room>". The Spotify account linked in the Sonos app plays.
- [ ] Playing an album, playlist, song, Liked Songs or an artist starts it in that room. Play, pause, skip, seek, volume, shuffle and repeat work.
- [ ] Changing the song or volume from the Sonos app or a phone shows on the player within a second (events, not polling).
- [ ] "Play on this player" switches back.

### Home Assistant (at home, with Home Assistant on the same network)

- [ ] With `--fake`, Home shows a pretend house with two pages, and taps change the tiles.
- [ ] `ziggy config` → Home Assistant offers the address it finds on the network, then saves the address and token. The player restarts.
- [ ] Home is second in the rail, under Search. The page chips match the pages in the config.
- [ ] Tapping a light that's off turns it on. On a light that's on, the left part turns it off and the strip on the right opens the brightness bar.
- [ ] Dragging the bar dims the real light. The bar goes back to the music 8 seconds after the last touch.
- [ ] Switches and fans toggle on a tap. A scene button runs the scene and stays outlined for 4 seconds.
- [ ] Changing a light in the Home Assistant app shows on its tile within a second.
- [ ] A control named in the config (`{ entity = "light.desk", name = "Desk lamp" }`) shows that name.
- [ ] An entity that isn't in Home Assistant shows a grey "Not in Home Assistant" tile.
- [ ] Stop Home Assistant: a "Can't reach Home Assistant" banner appears and the tiles grey out. Start it again: the player reconnects by itself.
- [ ] A wrong token shows "Home Assistant refused Ziggy's sign-in".
- [ ] A player without `[home_assistant]` in its config shows no Home item.

### 6. Settings

The gear at the bottom of the left rail opens Settings.

- [ ] It shows your Spotify account name and ID.
- [ ] The pencil next to the device name opens the keyboard. Saving says "Restarting the player…", the player restarts, and Spotify on your phone shows the new name.
- [ ] On the Mac, the Speaker section says speakers are paired on the Raspberry Pi. With `--fake`, "Find speakers" lists three pretend speakers; picking "Boombox" restarts on it, and "Old Speaker" fails with a message.
- [ ] Your choices are kept in `dev/state/settings.json`. Delete that file to go back to the config file's values.

## On the Pi

Once the Mac checks pass, follow section 2 of [docs/hardware-checklist.md](docs/hardware-checklist.md). In short:

1. **Flash the SD card.** Use Raspberry Pi Imager with Raspberry Pi OS Lite (64-bit). Set the hostname to `ziggy-<name>`, and set a user, Wi-Fi and your SSH key. Attach the display ribbon cable while the Pi is unplugged.
2. **Provision:** `scripts/provision.sh ziggy-<name>.local`, then reboot the Pi.
3. **Sign the account in** on the Mac:
   ```bash
   cargo run -p ziggy-setup --bin ziggy -- auth --state-dir secrets/<name>
   cargo run -p ziggy-setup --bin ziggy -- probe --state-dir secrets/<name>
   ```
4. **Configure:** `cp devices/example.toml devices/<name>.toml`, then set `device_name` in that file.
5. **Build and deploy:**
   ```bash
   scripts/build-pi.sh
   scripts/deploy.sh ziggy-<name>.local devices/<name>.toml secrets/<name>
   ```
6. **Pick the speaker** in Settings on the touchscreen if you use Bluetooth: Find speakers, then tap it. Put the speaker in pairing mode first.
7. **Run the release checks** in the checklist: cold boot, touch, audio, memory, idle, Wi-Fi drop, crash recovery and Bluetooth.

To read logs on the Pi:

```bash
ssh ziggy-<name>.local journalctl -u ziggy-player -n 50
```

