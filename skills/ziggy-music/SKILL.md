---
name: ziggy-music
description: Copy music files (MP3, M4A/AAC, FLAC) from this computer onto a Ziggy player's SD card with the `ziggy music` command. Use when someone asks to put albums, artists, songs or a folder of music on their Ziggy player, to see what music is on it, to check its free space, or to remove music from it.
---

# Copying music to a Ziggy player

A Ziggy player keeps music files on its SD card and plays them under My music. The `ziggy` command on this computer copies them there over SSH. Use only `ziggy music` commands to change the player's music. Never copy, move or delete files on the player any other way: no `ssh`, `scp`, `rsync` or the shared folder.

## 1. Find the player

- Ask which player, unless the person already said. The address is the hostname set when the SD card was made, with `.local` added, such as `ziggy-kitchen.local`. It may include a user, such as `pi@ziggy-kitchen.local`.
- To check the address works, run `ziggy status <host>`. If it can't connect, tell the person and stop.

## 2. Check the free space

Run `ziggy music space <host>`. It prints how much music is on the player and how much room is left.

## 3. Find the music

- Look where the person says. Otherwise look in `~/Music`.
- Find the folders that match what they named: an artist's folder, an album's folder, or single files. Use `ls` or `find` to look; don't change anything on this computer.
- Pick the folder for what they asked for. A folder is copied with its name, so copying `~/Music/Abba` puts it on the player as `Abba`, with everything inside.
- Don't copy all of `~/Music` unless they ask for everything. On a Mac, the Music app keeps its own files under `~/Music/Music/Media.localized/Music/`, in Artist/Album folders. Copy from there, not the app's library file.
- The player plays MP3, M4A/AAC and FLAC. Cover images named `cover.jpg`, `folder.jpg` or `front.jpg` (or `.png`) are copied too. Other files are skipped. Songs downloaded from Apple Music are protected `.m4p` files, which the player can't play; they're skipped.
- If you're not sure which folder they mean, show the choices and ask.

## 4. Copy

Run one command with every folder or file, then the host last:

```bash
ziggy music add "$HOME/Music/Abba/Gold" "$HOME/Music/Moana Soundtrack" ziggy-kitchen.local
```

Quote paths with spaces, and write `$HOME` rather than `~` inside quotes. The command checks the space first and stops with a message if the music won't fit; if so, tell the person how much is needed and how much is free, and ask what to leave out. Files already on the player are skipped, so running it again is safe. If copying stops part-way, run the same command again.

## 5. Report

Tell the person, briefly:

- what was copied, by folder name;
- any files skipped, and why (the command lists them);
- that it shows in My music on the player a few seconds after copying ends.
- if the command warned that My music is off, that they need to turn it on (Settings → My music on the touchscreen, or `ziggy config`) before the music shows.

## Other commands

- `ziggy music list <host>` lists the music on the player by folder, with sizes.
- `ziggy music remove "<folder>" <host>` deletes a folder from the player, such as `"Abba/Gold"`. Only remove music the person asked to remove. Name the folder and ask them to confirm first, then run it with `--yes`, since the command can't ask them itself from here.
