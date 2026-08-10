mod artwork;
mod config;
mod metadata;
mod pixoo;
mod state;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use state::{Action, Tracker};

const PLAYERCTL_FORMAT: &str = "{{playerName}}\t{{status}}\t{{mpris:artUrl}}";
const RESPAWN_DELAY: Duration = Duration::from_secs(2);
const TICK_INTERVAL: Duration = Duration::from_secs(1);
const FALLBACK_CHANNEL: u8 = 1;

/// Which channel to hand the screen back to. A configured value always wins;
/// otherwise the channel the device reported when artwork took over.
struct RestoreTarget {
    configured: Option<u8>,
    captured: u8,
}

impl RestoreTarget {
    fn new(configured: Option<u8>) -> Self {
        Self {
            configured,
            captured: FALLBACK_CHANNEL,
        }
    }

    fn current(&self) -> u8 {
        self.configured.unwrap_or(self.captured)
    }
}

fn main() -> Result<()> {
    let config = config::load()?;
    let client = pixoo::Client::new(&config.pixoo_ip);
    let mut tracker = Tracker::new(Duration::from_secs(config.idle_restore_secs));
    let mut restore = RestoreTarget::new(config.restore_channel);

    let rx = spawn_playerctl_reader()?;
    loop {
        match rx.recv_timeout(TICK_INTERVAL) {
            Ok(line) => {
                println!("recv: {line:?}");
                let np = metadata::parse_line_excluding(&line, &config.excluded_players);
                tracker.observe(np.as_ref(), Instant::now());
                // Drain the backlog so a slow HTTP action doesn't make us
                // draw artwork for tracks the user has already skipped past.
                while let Ok(line) = rx.try_recv() {
                    println!("recv: {line:?}");
                    let np = metadata::parse_line_excluding(&line, &config.excluded_players);
                    tracker.observe(np.as_ref(), Instant::now());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("playerctl reader thread died")
            }
        }
        if let Some(action) = tracker.decide(Instant::now()) {
            apply(&client, &mut tracker, &mut restore, action);
        }
    }
}

fn apply(
    client: &pixoo::Client,
    tracker: &mut Tracker,
    restore: &mut RestoreTarget,
    action: Action,
) {
    match action {
        Action::ShowArtwork {
            url,
            takes_ownership,
        } => {
            // Capture before drawing: drawing itself can alter what
            // Channel/GetIndex reports. A failed capture fails the whole
            // action — never take the screen without knowing how to give it
            // back — and the tracker retries after its backoff.
            if takes_ownership && restore.configured.is_none() {
                match client.get_index() {
                    Ok(index) => {
                        restore.captured = index;
                        println!("captured channel {index} to restore later");
                    }
                    Err(err) => {
                        eprintln!("failed: capturing current channel: {err:#}");
                        tracker.show_failed(Instant::now());
                        return;
                    }
                }
            }
            let result = artwork::load(&url)
                .and_then(|bytes| artwork::rgb888_scaled(&bytes, pixoo::SIZE))
                .and_then(|rgb| client.show_frame(&rgb));
            match result {
                Ok(()) => {
                    tracker.shown(&url);
                    println!("done: drew artwork {url}");
                }
                Err(err) => {
                    eprintln!("failed: drawing {url}: {err:#}");
                    tracker.show_failed(Instant::now());
                }
            }
        }
        Action::RestoreChannel => match client.restore_channel(restore.current()) {
            Ok(()) => {
                tracker.restored();
                println!("done: restored channel {}", restore.current());
            }
            Err(err) => {
                eprintln!("failed: restoring channel {}: {err:#}", restore.current());
                tracker.restore_failed(Instant::now());
            }
        },
    }
}

/// Streams `playerctl --follow` lines on a channel. An empty line means "no
/// active player". playerctl exits when no player is around (version
/// dependent), so keep respawning it — same strategy as the polybar script.
/// stderr is inherited so playerctl's own errors reach the daemon's log.
fn spawn_playerctl_reader() -> Result<mpsc::Receiver<String>> {
    which_playerctl()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || loop {
        match Command::new("playerctl")
            .args(["--follow", "metadata", "--format", PLAYERCTL_FORMAT])
            .stdout(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(stdout) = child.stdout.take() {
                    let mut reader = BufReader::new(stdout);
                    let mut buf = Vec::new();
                    loop {
                        buf.clear();
                        // read_until + lossy conversion: a non-UTF-8 byte in
                        // a metadata line must not kill the stream.
                        match reader.read_until(b'\n', &mut buf) {
                            Ok(0) => break,
                            Ok(_) => {
                                let line = String::from_utf8_lossy(&buf).trim_end().to_string();
                                if tx.send(line).is_err() {
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    return;
                                }
                            }
                            Err(err) => {
                                eprintln!("playerctl read error: {err}");
                                break;
                            }
                        }
                    }
                    // Kill before wait: on a read error the child may still
                    // be alive, and wait() on a live --follow blocks forever.
                    let _ = child.kill();
                }
                let _ = child.wait();
                eprintln!("playerctl exited; respawning in {RESPAWN_DELAY:?}");
            }
            Err(err) => eprintln!("failed to spawn playerctl: {err}"),
        }
        if tx.send(String::new()).is_err() {
            return;
        }
        std::thread::sleep(RESPAWN_DELAY);
    });
    Ok(rx)
}

fn which_playerctl() -> Result<()> {
    Command::new("playerctl")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("playerctl not found in PATH — install it first")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_restore_channel_wins_over_captured() {
        let mut r = RestoreTarget::new(Some(2));
        r.captured = 0;
        assert_eq!(r.current(), 2);
    }

    #[test]
    fn unconfigured_restore_uses_captured_channel() {
        let mut r = RestoreTarget::new(None);
        assert_eq!(r.current(), FALLBACK_CHANNEL);
        r.captured = 0;
        assert_eq!(r.current(), 0);
    }
}
