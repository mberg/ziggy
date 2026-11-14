# Music on the card — Design

Date: 2026-11-14
Status: Draft for review

## Goal

Ziggy plays music files kept on the player's own SD card (MP3, M4A/AAC, FLAC), alongside Spotify or without it.

- **Side by side.** Music on the card has its own item in the rail. Spotify sections are unchanged. Playing one pauses the other, the same way AirPlay works today.
- **Card-only players.** A player can be set up with no Spotify account at all. It then shows only the music on the card, and needs no Spotify Premium.
- **Getting music on.** Three ways, all landing in the same folder:
  - a shared folder that shows up in Finder or Windows;
  - a `ziggy music` command on your computer;
  - an agent skill that drives that command.

## Name in the rail

The rail item is **My music**.

## What exists to build on

- **Sound out.** AirPlay already sends raw audio to `pw-cat`, which plays it through PipeWire on the headphone jack or a Bluetooth speaker (`airplay/mod.rs`, `Speaker`). The local player uses the same path.
- **One source at a time.** AirPlay already pauses Spotify and takes over Now Playing (`app/airplay.rs`). My music follows the same rules.
- **Sections that appear when set up.** Books and Home are hidden until configured. My music is hidden while the card has no music. On a card-only player, the Spotify sections are hidden.
- **Covers that aren't URLs.** `images.rs` already draws covers for a made-up scheme (`fake-cover:`). Covers from the card use the same mechanism with a `local-cover:` scheme.
- **Play history.** `history.rs` keeps plays in SQLite. Songs from the card go in the same table, so Recent lists both kinds.
- **Audiobooks need the same player.** The Books page can't play yet ("Listening comes next"). The audiobooks design called for a local decoder and output; this is that player. Its first use is files on the card, and it later streams books from Audiobookshelf.

## The experience

### With Spotify

- **Rail:** a **My music** item, shown when the card has at least one song.
- **My music screen:** Albums and Artists tabs, with the existing grid and list toggle. Albums are sorted by artist, then by name.
- **Album page:** the cover and track list, with Play and Shuffle, like a Spotify album.
- **Artist page:** that artist's albums from the card.
- **Now Playing:** the same screen, marked "On this player" the way AirPlay is marked "AirPlay". Play, pause, skip, seek, shuffle, repeat, volume and the sleep timer all work.
- **Hand-off:** playing from My music pauses Spotify. Playing anything from Spotify, or picking the player in a Spotify app, stops My music. AirPlay starting stops My music too.
- **Search:** results gain a "On this player" row, matched on the device by song, album and artist names.
- **Recent:** albums from the card appear among Spotify's.
- **Not in the first version:** hearting, saving or adding a song from the card to a playlist. Those buttons are hidden for songs from the card.

### Card-only

- The rail shows My music, Search, Recent, Settings, and Books and Home if they're set up.
- The player opens on My music.
- With no music yet, My music shows how to add some: the `ziggy music add` command, and the shared folder's name if it's on (or where to turn it on).
- Settings has no Spotify items. Connection → Reconnect rejoins the Wi-Fi only.
- Spotify can be added later by running `ziggy setup` again and choosing it.

## Getting music onto the player

All three ways write to one folder on the Pi: `/var/lib/ziggy/music`. It is owned by the `ziggy-player` user, with a group the SSH user is added to, so both can write. Any folder layout works, because the library is read from the files' tags. `Artist/Album/01 Song.mp3` is suggested.

A 16 GB card has about 10 GB free after the system, which is roughly 2,000–2,500 songs at normal MP3 quality.

### The shared folder

- [Samba](https://www.samba.org/), the standard Mac and Windows file-sharing service, shares the folder as **Music** under the player's name, such as "Ziggy Kitchen". Avahi announces it, so it shows up in Finder's sidebar.
- **Off by default.** It's turned on in Settings → My music → Shared folder, or in `ziggy config`. `ziggy setup` offers to turn it on.
- **No password by default**, so a family can drag files in without accounts. Anyone on the home Wi-Fi can reach it. `ziggy config` can set a password.
- `smbd` only runs while the share is on, so players that don't use it pay nothing in memory on the Pi 3 A+, which has 512 MB.

### The `ziggy music` command

```
ziggy music add <files or folders> ziggy-kitchen.local
ziggy music list ziggy-kitchen.local
ziggy music remove <album or folder> ziggy-kitchen.local
ziggy music space ziggy-kitchen.local
```

- `add` copies with `rsync` over SSH, keeping each folder's layout and skipping files that are already there. It checks the free space first, and stops with a clear message if the music won't fit. It accepts the three supported formats and lists anything it skipped.
- `list` prints albums and artists as the player sees them, read from the player's index.
- `remove` deletes an album's folder.
- `space` shows how much room is left.
- Setup installs `rsync` on the Pi.

### The agent skill

A `skills/ziggy-music/SKILL.md` in the repo, for Claude Code and other agents that read skills. It tells an agent how to:

- find the player's address (`ziggy status`, or ask);
- check free space with `ziggy music space`;
- copy the albums, artists or folders the person names with `ziggy music add`, from their Music folder or a path they give;
- report what was added and what was skipped.

The skill only calls `ziggy music`. It never writes to the Pi by itself, so the command stays the single way in. The README says how to install it, for example by copying it into `~/.claude/skills`.

## The library on the player

- **Reading tags.** [`lofty`](https://crates.io/crates/lofty) reads title, artist, album artist, album, track and disc number, year and length from MP3, M4A and FLAC.
- **Covers.** The embedded picture comes first. Otherwise `cover.jpg`, `folder.jpg` or `front.jpg` in the album's folder. Otherwise the made-up gradient cover.
- **Missing tags.** Without tags, the folder names stand in for artist and album, and the file name stands in for the title.
- **Index.** Kept in SQLite next to the play history: one row per file, with its path, size, modified time and tags. Only new or changed files are read again.
- **Noticing changes.** A scan runs at start. While running, [`notify`](https://crates.io/crates/notify) (inotify on the Pi) watches the folder. Changes are batched and wait until copying has paused for a few seconds, so a half-copied album isn't added. A scan of a few thousand unchanged files only compares sizes and times.
- **IDs.** Songs are `local:track:<id>`, albums `local:album:<id>` and artists `local:artist:<id>`. Each ID is a short hash of the album artist and album name, so an album keeps its ID if its folder is renamed.
- **In the existing types.** The library is served as `Collection` and `Track` values, so the album and track screens are reused. A second `LibrarySource` implementation, `library/local.rs`, answers `section` and `tracks` for My music.

## The local player

A new `player/local` module, driven by the same `PlayerCommand`s as Soloist and Sonos, and answering with the same `PlayerUpdate`s.

- **Decoding:** [`symphonia`](https://crates.io/crates/symphonia) handles MP3, AAC in M4A, and FLAC. Audio is resampled to 44.1 kHz stereo if needed.
- **Output:** 16-bit stereo to `pw-cat`, as AirPlay does, with the same target for a Bluetooth speaker. The stream is closed when paused, so a Bluetooth speaker can sleep.
- **Queue:** the album's or search result's songs, with shuffle, repeat (off, all, one), next and previous. Previous restarts the song after 3 seconds, as Spotify does.
- **Seek:** done in the decoder.
- **Volume:** shares the player's volume setting, applied by scaling samples so the volume slider means the same as with Spotify. Whether PipeWire's own node volume is better is checked during the build.
- **Position:** reported every second for the scrubber, and written to history the same way as Spotify plays.
- **Gapless playback:** not in the first version.

### Core state

- `AppState` gets a `source: Spotify | Local | AirPlay`. The AirPlay flag becomes one case of it.
- Commands from Now Playing go to whichever source is playing.
- Sleep timer, screen wake and "playing keeps the screen awake" work for all three.

## Voice

- The Gemini tool that plays things gains the music on the card. Its `kind` list stays the same; a new `where` field takes `spotify`, `card` or `any`.
- **With Spotify on, Spotify comes first.** A plain "play Moana" plays it from Spotify. The card is searched only when asked for ("play Moana from my music"), or when Spotify finds nothing.
- **On a card-only player,** every request is matched against the card.
- "Pause", "next", "louder" and the sleep timer work as they do for Spotify.
- "Heart this" on a song from the card answers that it only works for Spotify songs.

## Card-only setup

- `ziggy setup` asks early: **Use Spotify on this player? (needs Spotify Premium)**.
- If no, it skips the Soloist key, the developer app and both sign-ins. Install leaves out Soloist and its service.
- The device config gets `spotify = false`. The player then starts neither Soloist nor the Web API library. It hides the Spotify sections, and doesn't show Spotify sign-in or rate-limit messages.
- Running `ziggy setup` again with "yes" adds Spotify. Music on the card stays.
- `ziggy signin` on a card-only player says Spotify isn't set up, and how to add it.

## Sonos

Not in the first version. A Sonos room can only play a file the Pi serves over HTTP. That is a later step: a small file server on the player, and the local queue sent to Sonos as URLs. Until then, with Sonos as the speaker, My music plays on the Pi's own output and says so. AirPlay has the same limit today.

## Phases

1. **The local player.** Decode, `pw-cat` output, queue, seek, volume, source hand-off with Spotify and AirPlay, and Now Playing marked "On this player". It plays a folder of test files on a Mac and on the Pi. *(the riskiest step: sharing the audio device with Soloist)*
2. **The library and My music.** Tag reading, the index, watching for changes, covers, the rail item, Albums and Artists, album and artist pages, search and Recent.
3. **Getting music on.** `ziggy music add/list/remove/space`, `rsync` and folder permissions in setup, the Samba share with its setting, the skill, and the README.
4. **Card-only players.** The setup question, `spotify = false`, hiding Spotify, the help shown when My music is empty, and voice on the card.
5. **Later, if wanted.** Playlists and favourites of your own for songs on the card; playing on Sonos; gapless playback; Audiobookshelf books through the same player.

## Risks

- **Two players and one speaker.** Soloist holds its PipeWire stream while paused. Phase 1 starts with a check on the Pi, including Bluetooth, that the local stream plays cleanly while Soloist is paused, and that Soloist resumes cleanly afterwards.
- **Memory on a Pi 3 A+ (512 MB).** Soloist, the player and Samba together need measuring. Samba only runs on players with the share turned on.
- **Large libraries.** The first scan of thousands of files on an SD card could take minutes. It runs in the background, My music fills in as it goes, and later scans only check sizes and times.
- **Half-copied files.** Covered by waiting for copying to pause, and by skipping files that won't decode, which are retried at the next change.
- **An open share.** Anyone on the home Wi-Fi can add or delete music while it's on. That's why it's off by default and can have a password.
- **Odd tags.** Compilations and "feat." artists are grouped by album artist. Without that tag, an album with mixed artists falls back to "Various artists" when its tracks share an album name and folder.

## Decisions (2026-11-14)

1. **Side by side,** not a separate mode.
2. **Both a copy command and a shared folder,** plus an agent skill that uses the command.
3. **Card-only players are supported,** with no Spotify account.
4. **The rail item is called "My music".**
5. **The shared folder is off by default,** and has no password unless one is set.
6. **Formats: MP3, M4A/AAC and FLAC.** Ogg and WAV are cheap to add later if wanted.
7. **Voice tries Spotify first** when Spotify is on. The card is used when asked for, when Spotify finds nothing, or on a card-only player.
