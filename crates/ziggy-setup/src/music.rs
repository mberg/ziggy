//! `ziggy music`: copies music files onto a player's SD card, and lists, removes and measures
//! them there. Files are copied with rsync over SSH into the music folder, the same folder the
//! shared folder and the player use.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use console::style;
use dialoguer::Password;
use dialoguer::theme::ColorfulTheme;

use crate::devconfig::{self, MUSIC_DIR};
use crate::install;
use crate::remote::{Remote, quote};
use crate::wizard::{connect, ok, say, yes};

/// Room left free on the card after copying, for the system, logs and the player's own files.
const KEEP_FREE: u64 = 300_000_000;

/// The formats the player plays.
const MUSIC: [&str; 4] = ["mp3", "m4a", "aac", "flac"];
/// Album covers the player reads from an album's folder.
const COVERS: [&str; 6] = [
    "cover.jpg",
    "cover.png",
    "folder.jpg",
    "folder.png",
    "front.jpg",
    "front.png",
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Music,
    Cover,
    /// Copied nowhere, and listed at the end.
    Other,
}

/// What a file is, from its name.
pub fn kind(path: &Path) -> Kind {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ext = Path::new(&name)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    if MUSIC.contains(&ext.as_str()) {
        Kind::Music
    } else if COVERS.contains(&name.as_str()) {
        Kind::Cover
    } else {
        Kind::Other
    }
}

/// One rsync run: the folder it runs from, and the files under it to copy, by their path
/// relative to it and their size.
#[derive(Debug, Default, PartialEq)]
pub struct Batch {
    pub base: PathBuf,
    pub files: Vec<(String, u64)>,
}

/// What `ziggy music add` will copy.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    pub batches: Vec<Batch>,
    /// Files left out: not music or a cover, or a link.
    pub skipped: Vec<PathBuf>,
    /// The names copied into the top of the music folder.
    pub tops: Vec<String>,
}

impl Plan {
    pub fn files(&self) -> impl Iterator<Item = &(String, u64)> {
        self.batches.iter().flat_map(|b| b.files.iter())
    }

    pub fn songs(&self) -> usize {
        self.files()
            .filter(|(p, _)| kind(Path::new(p)) == Kind::Music)
            .count()
    }

    /// The bytes still to copy, given what's on the player: a file at the same path with the
    /// same size is already there.
    pub fn to_copy(&self, on_player: &HashMap<String, u64>) -> (usize, u64) {
        self.files()
            .filter(|(path, size)| on_player.get(path) != Some(size))
            .fold((0, 0), |(n, bytes), (_, size)| (n + 1, bytes + size))
    }
}

/// Works out what to copy. A folder is copied by its name into the music folder, with what's
/// inside it; a file goes in the top of the music folder.
pub fn plan(paths: &[PathBuf]) -> anyhow::Result<Plan> {
    let mut plan = Plan::default();
    for path in paths {
        let full = std::fs::canonicalize(path)
            .with_context(|| format!("can't find {}", path.display()))?;
        let (Some(base), Some(name)) = (full.parent(), full.file_name()) else {
            bail!(
                "can't copy {}; name a folder or files in it",
                path.display()
            );
        };
        let name = name.to_string_lossy().into_owned();
        let mut batch = Batch {
            base: base.to_path_buf(),
            files: Vec::new(),
        };
        if full.is_dir() {
            walk(&full, &name, &mut batch.files, &mut plan.skipped)?;
        } else if kind(&full) == Kind::Other {
            plan.skipped.push(full.clone());
        } else {
            batch
                .files
                .push((name.clone(), std::fs::metadata(&full)?.len()));
        }
        if !batch.files.is_empty() {
            if !plan.tops.contains(&name) {
                plan.tops.push(name);
            }
            plan.batches.push(batch);
        }
    }
    Ok(plan)
}

/// Adds the music and covers under `dir` (known as `rel`) to `files`, in name order. Hidden
/// files, such as a Mac's .DS_Store, are left out quietly; links are listed as skipped.
fn walk(
    dir: &Path,
    rel: &str,
    files: &mut Vec<(String, u64)>,
    skipped: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)?;
        let rel = format!("{rel}/{name}");
        if meta.is_dir() {
            walk(&path, &rel, files, skipped)?;
        } else if meta.is_file() && kind(&path) != Kind::Other {
            files.push((rel, meta.len()));
        } else {
            skipped.push(path);
        }
    }
    Ok(())
}

/// The ssh command rsync uses: the same options and config as the rest of `ziggy`.
fn ssh_command() -> String {
    let mut ssh = String::from("ssh -o BatchMode=yes -o ConnectTimeout=10");
    if let Some(config) = std::env::var_os("ZIGGY_SSH_CONFIG") {
        ssh.push_str(&format!(" -F {}", quote(&config.to_string_lossy())));
    }
    ssh
}

/// Copies a batch's files, keeping their folders. rsync skips a file already there with the
/// same size and time, and keeps a half-copied file so a second run carries on. Folder times
/// aren't set, since a folder made through the shared folder belongs to the player.
pub fn rsync_command(host: &str, base: &Path) -> Command {
    let mut c = Command::new("rsync");
    c.current_dir(base).args([
        "-rt",
        "--omit-dir-times",
        "--partial",
        "--progress",
        "--from0",
        "--files-from=-",
        "-e",
        &ssh_command(),
        ".",
        &format!("{host}:{MUSIC_DIR}/"),
    ]);
    c
}

fn rsync(host: &str, batch: &Batch) -> anyhow::Result<()> {
    let mut child = rsync_command(host, &batch.base)
        .stdin(Stdio::piped())
        .spawn()
        .context("running rsync; is it installed on this computer?")?;
    let mut list = Vec::new();
    for (path, _) in &batch.files {
        list.extend_from_slice(path.as_bytes());
        list.push(0);
    }
    child.stdin.take().expect("stdin").write_all(&list)?;
    let status = child.wait()?;
    if !status.success() {
        bail!(
            "copying stopped ({status}). Run the same command again to carry on where it stopped"
        );
    }
    Ok(())
}

/// Lets the player and the shared folder change what was just copied: folders 2775 (group
/// ziggy-music, kept for new folders inside), files 664. Only the SSH user's own files change.
pub fn permissions_script(tops: &[String]) -> String {
    let tops: Vec<String> = tops.iter().map(|t| format!("./{}", quote(t))).collect();
    format!(
        "cd {MUSIC_DIR}\n\
         find {} -user \"$(id -un)\" \\( -type d -exec chmod 2775 {{}} + \\) \
           -o \\( -type f -exec chmod 664 {{}} + \\) 2>/dev/null || true",
        tops.join(" ")
    )
}

/// Whether the player is ready for `ziggy music add`.
fn ready(r: &dyn Remote, host: &str) -> anyhow::Result<()> {
    let out = r.run(&format!(
        "if [ ! -d {MUSIC_DIR} ] || ! command -v rsync >/dev/null; then echo missing\n\
         elif [ ! -w {MUSIC_DIR} ]; then echo locked\n\
         else echo ready; fi"
    ))?;
    match out.trim() {
        "ready" => Ok(()),
        "locked" => bail!(
            "this SSH user can't write to the music folder. Run `ziggy update {host}` to allow it"
        ),
        _ => bail!("this player has no music folder yet. Run `ziggy update {host}` first"),
    }
}

/// The files in the music folder: path inside it, and size.
pub fn on_player(r: &dyn Remote) -> anyhow::Result<HashMap<String, u64>> {
    let out = r.run(&format!(
        "sudo find {MUSIC_DIR} -mindepth 1 -type f -printf '%s\\t%P\\0'"
    ))?;
    Ok(parse_listing(&out).into_iter().collect())
}

/// `find -printf '%s\t%P\0'`: a size, a tab, then the path, for each file. Hidden files, such
/// as the `._` files a Mac leaves through the shared folder, are left out.
pub fn parse_listing(out: &str) -> Vec<(String, u64)> {
    out.split('\0')
        .filter_map(|entry| {
            let (size, path) = entry.split_once('\t')?;
            if path.split('/').any(|part| part.starts_with('.')) {
                return None;
            }
            Some((path.to_string(), size.trim().parse().ok()?))
        })
        .collect()
}

/// The size of the card and the space free, in bytes, from `df -B1 --output=size,avail`.
pub fn parse_df(out: &str) -> Option<(u64, u64)> {
    let line = out.lines().rev().find(|l| !l.trim().is_empty())?;
    let mut numbers = line.split_whitespace().map(|n| n.parse::<u64>());
    match (numbers.next()?, numbers.next()?) {
        (Ok(size), Ok(free)) => Some((size, free)),
        _ => None,
    }
}

fn disk(r: &dyn Remote) -> anyhow::Result<(u64, u64)> {
    let out = r.run(&format!("df -B1 --output=size,avail {MUSIC_DIR}"))?;
    parse_df(&out).with_context(|| format!("couldn't read the free space: {out}"))
}

/// Sizes the way Finder shows them: 1 GB is 1,000,000,000 bytes.
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else if b >= 1e3 {
        format!("{:.0} KB", b / 1e3)
    } else {
        format!("{bytes} bytes")
    }
}

fn songs(n: usize) -> String {
    format!("{n} song{}", if n == 1 { "" } else { "s" })
}

/// A folder in the listing: its songs and bytes, all the way down.
#[derive(Debug, Default, PartialEq)]
pub struct Folder {
    pub songs: usize,
    pub bytes: u64,
    /// The folders one level down, such as an artist's albums.
    pub inside: BTreeMap<String, (usize, u64)>,
}

/// The music folder by its top two levels (often Artist / Album). Files at the top are under
/// the empty name.
pub fn folders(files: &[(String, u64)]) -> BTreeMap<String, Folder> {
    let mut out: BTreeMap<String, Folder> = BTreeMap::new();
    for (path, bytes) in files {
        let song = usize::from(kind(Path::new(path)) == Kind::Music);
        let parts: Vec<&str> = path.split('/').collect();
        let top = if parts.len() > 1 { parts[0] } else { "" };
        let folder = out.entry(top.to_string()).or_default();
        folder.songs += song;
        folder.bytes += bytes;
        if parts.len() > 2 {
            let inner = folder.inside.entry(parts[1].to_string()).or_default();
            inner.0 += song;
            inner.1 += bytes;
        }
    }
    out
}

/// Checks a folder named for `ziggy music remove` and gives its path inside the music folder.
/// It must be inside the music folder: no `..`, and no other absolute path.
pub fn folder_path(input: &str) -> Result<String, String> {
    let input = input.trim();
    let inside = match input.strip_prefix(MUSIC_DIR) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => rest,
        _ if input.starts_with('/') || input.starts_with('~') => {
            return Err(format!(
                "name a folder inside the music folder, such as \"Artist/Album\", not {input}"
            ));
        }
        _ => input,
    };
    let parts: Vec<&str> = inside
        .split('/')
        .filter(|p| !p.is_empty() && *p != ".")
        .collect();
    if parts.contains(&"..") {
        return Err("the folder can't contain \"..\"".into());
    }
    if parts.is_empty() {
        return Err("name a folder inside the music folder, such as \"Artist/Album\"".into());
    }
    Ok(parts.join("/"))
}

/// `ziggy music add`.
pub fn add(paths: &[PathBuf], host: &str) -> anyhow::Result<()> {
    let plan = plan(paths)?;
    if plan.batches.is_empty() {
        show_skipped(&plan.skipped);
        bail!("nothing to copy. The player plays MP3, M4A, AAC and FLAC files");
    }
    let ssh = connect(host)?;
    ready(&ssh, host)?;
    let my_music = install::read_config(&ssh)
        .ok()
        .flatten()
        .is_some_and(|t| devconfig::my_music_on(&t));
    if !my_music {
        say(&format!(
            "{} My music is off in this player's config, so the music copied now won't show.\n\
             Turn it on in Settings → My music on the touchscreen, or with `ziggy config {host}`.\n\
             (A choice made on the touchscreen comes first.)",
            style("!").yellow()
        ));
    }
    let (count, bytes) = plan.to_copy(&on_player(&ssh)?);
    if count == 0 {
        ok(&format!(
            "All {} are already on the player",
            songs(plan.songs())
        ));
        show_skipped(&plan.skipped);
        return Ok(());
    }
    let (_, free) = disk(&ssh)?;
    if bytes + KEEP_FREE > free {
        bail!(
            "that's {} to copy, and the player has {} free (it keeps {} for itself). \
             Copy fewer albums, or make room with `ziggy music remove`",
            size(bytes),
            size(free),
            size(KEEP_FREE)
        );
    }
    say(&format!(
        "Copying {count} file{} ({}) to {host}…",
        if count == 1 { "" } else { "s" },
        size(bytes)
    ));
    for batch in &plan.batches {
        rsync(host, batch)?;
    }
    ssh.run(&permissions_script(&plan.tops))
        .context("letting the player change the new files")?;
    ok(&format!(
        "Copied {}. They show in My music a few seconds after copying ends",
        plan.tops.join(", ")
    ));
    show_skipped(&plan.skipped);
    Ok(())
}

fn show_skipped(skipped: &[PathBuf]) {
    if skipped.is_empty() {
        return;
    }
    say(&format!(
        "{} Skipped {} file{} that aren't MP3, M4A, AAC or FLAC music or a cover:",
        style("!").yellow(),
        skipped.len(),
        if skipped.len() == 1 { "" } else { "s" }
    ));
    for path in skipped.iter().take(20) {
        say(&format!("  {}", path.display()));
    }
    if skipped.len() > 20 {
        say(&format!("  and {} more", skipped.len() - 20));
    }
}

/// `ziggy music list`.
pub fn list(host: &str) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    let files: Vec<(String, u64)> = on_player(&ssh)?.into_iter().collect();
    if files.is_empty() {
        say(&format!(
            "There's no music on {host} yet. Copy some with `ziggy music add <folders> {host}`."
        ));
        return Ok(());
    }
    let folders = folders(&files);
    let width = folders
        .iter()
        .flat_map(|(name, f)| {
            std::iter::once(name.chars().count())
                .chain(f.inside.keys().map(|k| k.chars().count() + 2))
        })
        .max()
        .unwrap_or(0)
        .clamp(12, 48);
    for (name, folder) in &folders {
        let name = if name.is_empty() {
            "(top of the music folder)"
        } else {
            name
        };
        say(&format!(
            "{:<width$}  {}, {}",
            style(name).bold(),
            songs(folder.songs),
            size(folder.bytes)
        ));
        for (inner, (n, bytes)) in &folder.inside {
            say(&format!(
                "  {:<w$}  {}, {}",
                inner,
                songs(*n),
                size(*bytes),
                w = width - 2
            ));
        }
    }
    let (n, bytes) = folders
        .values()
        .fold((0, 0), |(n, b), f| (n + f.songs, b + f.bytes));
    say(&format!("\n{} in all, {}.", songs(n), size(bytes)));
    Ok(())
}

/// `ziggy music remove`.
pub fn remove(folder: &str, host: &str, confirmed: bool) -> anyhow::Result<()> {
    let path = folder_path(folder).map_err(anyhow::Error::msg)?;
    let ssh = connect(host)?;
    let full = format!("{MUSIC_DIR}/{path}");
    let there = ssh
        .run(&format!(
            "sudo test -e {} && echo yes || true",
            quote(&full)
        ))?
        .trim()
        == "yes";
    if !there {
        bail!(
            "there's no \"{path}\" in the music folder. `ziggy music list {host}` shows what's there"
        );
    }
    let inside = on_player(&ssh)?
        .into_iter()
        .filter(|(p, _)| p == &path || p.starts_with(&format!("{path}/")))
        .fold((0, 0), |(n, b), (p, size)| {
            (
                n + usize::from(kind(Path::new(&p)) == Kind::Music),
                b + size,
            )
        });
    if !confirmed
        && !yes(
            &format!(
                "Delete \"{path}\" ({}, {}) from {host}? This can't be undone",
                songs(inside.0),
                size(inside.1)
            ),
            false,
        )?
    {
        say("Nothing deleted.");
        return Ok(());
    }
    ssh.run(&format!("sudo rm -rf -- {}", quote(&full)))?;
    ok(&format!("Deleted \"{path}\""));
    Ok(())
}

/// `ziggy music space`.
pub fn space(host: &str) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    let files: Vec<(String, u64)> = on_player(&ssh)?.into_iter().collect();
    let (n, bytes) = files.iter().fold((0, 0), |(n, b), (p, size)| {
        (n + usize::from(kind(Path::new(p)) == Kind::Music), b + size)
    });
    let (card, free) = disk(&ssh)?;
    say(&format!("Music on {host}: {}, {}", songs(n), size(bytes)));
    say(&format!(
        "Free: {} of {} (the player keeps {} of it for itself)",
        size(free),
        size(card),
        size(KEEP_FREE)
    ));
    Ok(())
}

/// `ziggy music share-password`: sets or removes the shared folder's password.
pub fn share_password(host: &str) -> anyhow::Result<()> {
    let ssh = connect(host)?;
    let installed = ssh
        .run("dpkg -s samba >/dev/null 2>&1 && echo yes || true")?
        .trim()
        == "yes";
    if !installed {
        bail!("this player can't share its music folder yet. Run `ziggy update {host}` first");
    }
    say(
        "With a password, the shared folder asks for a name and password: the name is \"ziggy\".\n\
         Without one, anyone on your Wi-Fi can add or delete music while sharing is on.",
    );
    let password = Password::with_theme(&ColorfulTheme::default())
        .with_prompt("Password (empty removes the password)")
        .with_confirmation("The same password again", "The passwords don't match")
        .allow_empty_password(true)
        .interact()?;
    let password = Some(password.trim()).filter(|p| !p.is_empty());
    install::set_share_password(&ssh, password)?;
    if password.is_some() {
        ok("The shared folder now asks for a password. In Finder, connect as \"ziggy\"");
    } else {
        ok("The shared folder has no password");
    }
    let sharing = install::read_config(&ssh)
        .ok()
        .flatten()
        .is_some_and(|t| devconfig::music_share_on(&t));
    if !sharing {
        say(&format!(
            "Sharing is off in the config. Turn it on in Settings → My music → Shared folder,\n\
             or with `ziggy config {host}`. (A choice made on the touchscreen comes first.)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::FakeRemote;

    #[test]
    fn only_music_and_covers_are_copied() {
        for name in ["a.mp3", "B.MP3", "c.m4a", "d.aac", "e.flac", "Abba/f.FLAC"] {
            assert_eq!(kind(Path::new(name)), Kind::Music, "{name}");
        }
        for name in ["Album/cover.jpg", "Folder.JPG", "front.png"] {
            assert_eq!(kind(Path::new(name)), Kind::Cover, "{name}");
        }
        for name in [
            "a.ogg",
            "a.wav",
            "notes.txt",
            "back.jpg",
            "mp3",
            "cover.jpeg.txt",
        ] {
            assert_eq!(kind(Path::new(name)), Kind::Other, "{name}");
        }
    }

    #[test]
    fn a_folder_is_copied_by_name_and_a_file_to_the_top() {
        let dir = tempfile::tempdir().unwrap();
        let album = dir.path().join("Abba/Gold");
        std::fs::create_dir_all(&album).unwrap();
        std::fs::write(album.join("01 Dancing Queen.mp3"), b"12345").unwrap();
        std::fs::write(album.join("cover.jpg"), b"jpg").unwrap();
        std::fs::write(album.join("notes.txt"), b"x").unwrap();
        std::fs::write(album.join(".DS_Store"), b"x").unwrap();
        let loose = dir.path().join("Single.m4a");
        std::fs::write(&loose, b"1234567").unwrap();
        let other = dir.path().join("video.mov");
        std::fs::write(&other, b"x").unwrap();

        let plan = plan(&[dir.path().join("Abba/"), loose.clone(), other.clone()]).unwrap();
        assert_eq!(plan.tops, ["Abba", "Single.m4a"]);
        assert_eq!(plan.batches.len(), 2);
        let base = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(plan.batches[0].base, base);
        assert_eq!(
            plan.batches[0].files,
            [
                ("Abba/Gold/01 Dancing Queen.mp3".to_string(), 5),
                ("Abba/Gold/cover.jpg".to_string(), 3)
            ]
        );
        assert_eq!(plan.batches[1].files, [("Single.m4a".to_string(), 7)]);
        assert_eq!(plan.songs(), 2);
        assert_eq!(
            plan.skipped,
            [base.join("Abba/Gold/notes.txt"), base.join("video.mov")],
            "hidden files are left out quietly"
        );

        let mut there = HashMap::new();
        assert_eq!(plan.to_copy(&there), (3, 15));
        there.insert("Abba/Gold/01 Dancing Queen.mp3".to_string(), 5);
        there.insert("Single.m4a".to_string(), 1);
        assert_eq!(
            plan.to_copy(&there),
            (2, 10),
            "a different size is copied again"
        );

        assert!(super::plan(&[dir.path().join("missing")]).is_err());
    }

    #[test]
    fn rsync_keeps_folders_and_goes_into_the_music_folder() {
        let c = rsync_command("pi@ziggy-den.local", Path::new("/Users/me/Music"));
        let args: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(c.get_program(), "rsync");
        assert_eq!(c.get_current_dir(), Some(Path::new("/Users/me/Music")));
        for arg in [
            "-rt",
            "--omit-dir-times",
            "--partial",
            "--from0",
            "--files-from=-",
        ] {
            assert!(args.contains(&arg.to_string()), "{arg}: {args:?}");
        }
        assert!(args.iter().any(|a| a.starts_with("ssh -o BatchMode=yes")));
        assert_eq!(
            &args[args.len() - 2..],
            [".", "pi@ziggy-den.local:/var/lib/ziggy-music/"]
        );
    }

    #[test]
    fn copied_files_are_opened_up_to_the_player() {
        let script = permissions_script(&["Abba".into(), "It's -here.mp3".into()]);
        assert!(script.starts_with("cd /var/lib/ziggy-music\n"), "{script}");
        assert!(script.contains("find ./'Abba' ./'It'\\''s -here.mp3' -user \"$(id -un)\""));
        assert!(script.contains("-type d -exec chmod 2775 {} +"), "{script}");
        assert!(script.contains("-type f -exec chmod 664 {} +"), "{script}");
    }

    #[test]
    fn the_listing_and_free_space_are_read() {
        let out = "5000000\tAbba/Gold/01 Dancing Queen.mp3\x003000000\tAbba/Gold/02 SOS.mp3\x00\
                   100000\tAbba/Gold/cover.jpg\x002000000\tAbba/Waterloo.flac\x00\
                   4000000\tLoose.m4a\x004096\tAbba/Gold/._01 Dancing Queen.mp3\x00";
        let files = parse_listing(out);
        assert_eq!(files.len(), 5, "hidden files are left out");
        assert_eq!(files[3], ("Abba/Waterloo.flac".to_string(), 2_000_000));
        let folders = folders(&files);
        assert_eq!(folders["Abba"].songs, 3);
        assert_eq!(folders["Abba"].bytes, 10_100_000);
        assert_eq!(folders["Abba"].inside["Gold"], (2, 8_100_000));
        assert_eq!(folders[""].songs, 1, "files at the top");
        assert!(parse_listing("").is_empty());

        let df = "     1B-blocks        Avail\n 15563157504  9123456789\n";
        assert_eq!(parse_df(df), Some((15_563_157_504, 9_123_456_789)));
        assert_eq!(parse_df("Size Avail\n"), None);
        assert_eq!(parse_df(""), None);
    }

    #[test]
    fn sizes_read_like_finder() {
        assert_eq!(size(9_123_456_789), "9.1 GB");
        assert_eq!(size(80_400_000), "80 MB");
        assert_eq!(size(3_000), "3 KB");
        assert_eq!(size(12), "12 bytes");
    }

    #[test]
    fn only_folders_inside_the_music_folder_can_be_removed() {
        assert_eq!(folder_path("Abba/Gold").as_deref(), Ok("Abba/Gold"));
        assert_eq!(folder_path(" ./Abba//Gold/ ").as_deref(), Ok("Abba/Gold"));
        assert_eq!(
            folder_path("/var/lib/ziggy-music/Abba").as_deref(),
            Ok("Abba")
        );
        for bad in [
            "",
            "/",
            ".",
            "../etc",
            "Abba/../../etc",
            "/etc/passwd",
            "/var/lib/ziggy-music",
            "/var/lib/ziggy-music/",
            "/var/lib/ziggy-musicx/a",
            "~/Music",
        ] {
            assert!(folder_path(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_player_must_have_a_music_folder_it_lets_this_user_write() {
        let ready_pi = FakeRemote {
            output: "ready\n".into(),
            ..Default::default()
        };
        ready(&ready_pi, "den.local").unwrap();
        assert!(
            ready_pi
                .all_scripts()
                .contains("[ ! -w /var/lib/ziggy-music ]")
        );
        let locked = FakeRemote {
            output: "locked\n".into(),
            ..Default::default()
        };
        let e = ready(&locked, "den.local").unwrap_err().to_string();
        assert!(e.contains("ziggy update den.local"), "{e}");
        let old = FakeRemote::default();
        assert!(ready(&old, "den.local").is_err());
    }
}
