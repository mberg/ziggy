//! Running commands on the Pi over SSH, using the computer's own SSH setup and keys.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

/// What the installer needs from a device. `Ssh` is the real one; tests use a fake.
pub trait Remote {
    /// Runs a shell script on the device and returns what it printed.
    fn run(&self, script: &str) -> anyhow::Result<String>;
    /// Writes a file on the device as root, with a mode and optional owner.
    fn upload(
        &self,
        bytes: &[u8],
        path: &str,
        mode: &str,
        owner: Option<&str>,
    ) -> anyhow::Result<()>;
    /// A file's contents, or None if it doesn't exist.
    fn read(&self, path: &str) -> anyhow::Result<Option<String>>;
}

pub struct Ssh {
    pub host: String,
}

impl Ssh {
    pub fn new(host: &str) -> Self {
        Self {
            host: host.to_string(),
        }
    }

    fn command(&self, remote: &str) -> Command {
        let mut c = Command::new("ssh");
        // ZIGGY_SSH_CONFIG points ssh at another config file, e.g. for a test machine.
        if let Some(config) = std::env::var_os("ZIGGY_SSH_CONFIG") {
            c.arg("-F").arg(config);
        }
        c.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            &self.host,
            remote,
        ]);
        c
    }

    fn with_input(&self, remote: &str, input: &[u8]) -> anyhow::Result<String> {
        let mut child = self
            .command(remote)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("running ssh")?;
        child.stdin.take().expect("stdin").write_all(input)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!(
                "on {}: {}",
                self.host,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Whether the device answers over SSH without a password prompt.
    pub fn reachable(&self) -> bool {
        self.command("true")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Runs a command with its output shown live (for logs).
    pub fn stream(&self, remote: &str) -> anyhow::Result<()> {
        let mut c = Command::new("ssh");
        if let Some(config) = std::env::var_os("ZIGGY_SSH_CONFIG") {
            c.arg("-F").arg(config);
        }
        c.args(["-t", &self.host, remote]);
        c.status()?;
        Ok(())
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl Remote for Ssh {
    fn run(&self, script: &str) -> anyhow::Result<String> {
        self.with_input("bash -se", script.as_bytes())
    }

    fn upload(
        &self,
        bytes: &[u8],
        path: &str,
        mode: &str,
        owner: Option<&str>,
    ) -> anyhow::Result<()> {
        self.with_input(&upload_script(path, mode, owner), bytes)?;
        Ok(())
    }

    fn read(&self, path: &str) -> anyhow::Result<Option<String>> {
        let out = self.with_input(
            &format!("sudo cat {} 2>/dev/null || true", quote(path)),
            b"",
        )?;
        Ok((!out.is_empty()).then_some(out))
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// Against a real machine: `ZIGGY_TEST_HOST=… [ZIGGY_SSH_CONFIG=…] cargo test -- --ignored live_ssh`.
    #[test]
    #[ignore]
    fn live_ssh() {
        let Ok(host) = std::env::var("ZIGGY_TEST_HOST") else {
            return;
        };
        let ssh = Ssh::new(&host);
        assert!(ssh.reachable());
        let path = "/tmp/ziggy test/it's here.txt";
        ssh.upload(b"line one\nline 'two'\n", path, "640", None)
            .unwrap();
        assert_eq!(
            ssh.read(path).unwrap().as_deref(),
            Some("line one\nline 'two'\n")
        );
        assert_eq!(ssh.read("/tmp/ziggy-nothing-here").unwrap(), None);
        let out = ssh
            .run("set -e\nstat -c %a \"/tmp/ziggy test/it's here.txt\"\necho \"user=$(whoami)\"")
            .unwrap();
        assert!(out.contains("640"), "{out}");
        assert!(ssh.run("exit 3").is_err(), "a failing script is an error");
        assert!(!Ssh::new("nowhere.invalid").reachable());
    }
}

/// Writes stdin to `path` so that losing power part-way can never leave a broken file: the new
/// copy is written beside it and saved to the card, then swapped in with a rename, which is
/// all or nothing. Until then the old file stays as it was.
fn upload_script(path: &str, mode: &str, owner: Option<&str>) -> String {
    let owner = owner.map_or(String::new(), |o| format!("-o {o} -g {o} "));
    let new = quote(&format!("{path}.new"));
    let path = quote(path);
    format!(
        "sudo install -D -m {mode} {owner}/dev/stdin {new} && sudo sync {new} \
         && sudo mv -f {new} {path} && sudo sync"
    )
}

#[cfg(test)]
mod upload_tests {
    use super::*;

    #[test]
    fn uploads_are_saved_then_swapped_in() {
        let script = upload_script("/usr/local/bin/ziggy-player", "755", None);
        let write = script.find("install -D -m 755 /dev/stdin '/usr/local/bin/ziggy-player.new'");
        let save = script.find("sync '/usr/local/bin/ziggy-player.new'");
        let swap =
            script.find("mv -f '/usr/local/bin/ziggy-player.new' '/usr/local/bin/ziggy-player'");
        assert!(
            write.is_some() && save.is_some() && swap.is_some(),
            "{script}"
        );
        assert!(write < save && save < swap, "{script}");
        assert!(
            upload_script("/var/lib/ziggy/web-auth.json", "600", Some("ziggy-player"))
                .contains("-o ziggy-player -g ziggy-player /dev/stdin")
        );
    }
}

#[cfg(test)]
pub mod fake {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::Remote;

    /// A written file: its bytes, mode and owner.
    pub type FakeFile = (Vec<u8>, String, Option<String>);

    /// Records what would be done on a device.
    #[derive(Default)]
    pub struct FakeRemote {
        pub scripts: RefCell<Vec<String>>,
        pub files: RefCell<HashMap<String, FakeFile>>,
        /// Printed by every script, e.g. to say a reboot is needed.
        pub output: String,
    }

    impl Remote for FakeRemote {
        fn run(&self, script: &str) -> anyhow::Result<String> {
            self.scripts.borrow_mut().push(script.to_string());
            Ok(self.output.clone())
        }

        fn upload(
            &self,
            bytes: &[u8],
            path: &str,
            mode: &str,
            owner: Option<&str>,
        ) -> anyhow::Result<()> {
            self.files.borrow_mut().insert(
                path.to_string(),
                (bytes.to_vec(), mode.to_string(), owner.map(str::to_string)),
            );
            Ok(())
        }

        fn read(&self, path: &str) -> anyhow::Result<Option<String>> {
            Ok(self
                .files
                .borrow()
                .get(path)
                .map(|(b, _, _)| String::from_utf8_lossy(b).into_owned()))
        }
    }

    impl FakeRemote {
        pub fn file(&self, path: &str) -> Option<String> {
            self.read(path).unwrap()
        }

        pub fn all_scripts(&self) -> String {
            self.scripts.borrow().join("\n---\n")
        }
    }
}
