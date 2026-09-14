//! Follows what a HomePod on the LAN is playing, via pyatv's command line
//! tools.
//!
//! HomeKit itself carries no track metadata, and no Rust crate speaks the
//! protocol that does (MRP tunneled over AirPlay) — pyatv is the only mature
//! implementation. So this module treats `atvscript` the way the rest of the
//! daemon treats `playerctl`: a subprocess that streams state changes, parsed
//! line by line. Artwork is the one thing pyatv hands over as bytes rather
//! than a URL, so `atvremote artwork_save` writes it into a cache directory
//! and the file:// URL takes the place of an MPRIS `mpris:artUrl`.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use url::Url;

use crate::config::HomePod;
use crate::metadata::{NowPlaying, PlayStatus};
use crate::source::SourceId;

const SCRIPT_BIN: &str = "atvscript";
const REMOTE_BIN: &str = "atvremote";

/// Longer than the playerctl respawn: reconnecting means rediscovering a
/// device on the network, and a HomePod that is simply off should not be
/// hammered.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// How many artwork files to keep around. The current one is still being read
/// by the drawing code when the next track arrives, so one spare is the
/// minimum that is safe.
const KEEP_ARTWORK_FILES: u64 = 2;

/// One line of `atvscript push_updates`.
#[derive(Debug, Deserialize)]
struct ScriptLine {
    result: String,
    device_state: Option<String>,
    /// pyatv's identifier for the item being played; changes on track change.
    hash: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Update {
    State {
        status: PlayStatus,
        hash: Option<String>,
    },
    /// The connection to the device failed or went away.
    Gone,
}

/// Parses one line of `atvscript --protocol airplay push_updates`. Returns
/// None for the lines that say nothing about playback (power state, output
/// devices) and for anything unparseable.
pub fn parse_line(line: &str) -> Option<Update> {
    let parsed: ScriptLine = serde_json::from_str(line.trim()).ok()?;
    if parsed.result != "success" {
        return Some(Update::Gone);
    }
    let status = match parsed.device_state.as_deref()? {
        // Seeking and loading are on their way to playing; treating them as
        // Playing keeps the artwork up during a scrub instead of starting an
        // idle countdown.
        "playing" | "seeking" | "loading" => PlayStatus::Playing,
        "paused" => PlayStatus::Paused,
        "stopped" | "idle" => PlayStatus::Stopped,
        _ => return None,
    };
    Some(Update::State {
        status,
        hash: parsed.hash,
    })
}

/// The arguments that address one device, shared by both pyatv tools.
fn device_args(cfg: &HomePod) -> Vec<String> {
    let mut args = vec![
        "--id".to_string(),
        cfg.identifier.clone(),
        // A HomePod exposes its metadata through AirPlay; without this pyatv
        // also tries Companion, which it cannot pair with.
        "--protocol".to_string(),
        "airplay".to_string(),
    ];
    if let Some(address) = &cfg.address {
        args.push("--scan-hosts".to_string());
        args.push(address.clone());
    }
    args
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("pixoo-nowplaying")
}

/// Saves each item's artwork to disk and reports it as a file:// URL.
struct ArtworkCache {
    dir: PathBuf,
    device_args: Vec<String>,
    /// The last item asked about and the URL published for it — None when
    /// that item turned out to have no artwork. Misses are remembered as
    /// firmly as hits: the device pushes the same item repeatedly while it
    /// plays, and a stream with no cover art would otherwise cost an
    /// atvremote process on every push.
    answer: Option<(String, Option<String>)>,
    seq: u64,
}

impl ArtworkCache {
    fn new(dir: PathBuf, device_args: Vec<String>) -> Self {
        Self {
            dir,
            device_args,
            answer: None,
            seq: 0,
        }
    }

    /// URL for the item identified by `hash`, downloading it the first time
    /// that item is seen. None when the device has no artwork to give.
    fn url_for(&mut self, hash: &str) -> Option<String> {
        self.url_for_with(hash, Self::fetch)
    }

    /// Forgets what the device answered, so the next ask is a real one. Used
    /// after a reconnect: a miss caused by the connection dropping rather
    /// than by the item itself deserves another try.
    fn reset(&mut self) {
        self.answer = None;
    }

    fn url_for_with<F>(&mut self, hash: &str, fetch: F) -> Option<String>
    where
        F: FnOnce(&mut Self) -> Result<String>,
    {
        if let Some((asked, url)) = &self.answer {
            if asked == hash {
                return url.clone();
            }
        }
        let url = match fetch(self) {
            Ok(url) => Some(url),
            Err(err) => {
                // Routine for AirPlay streams from third-party apps, which
                // the HomePod plays without exposing any cover art.
                eprintln!("no artwork for the HomePod's current item: {err:#}");
                None
            }
        };
        self.answer = Some((hash.to_string(), url.clone()));
        url
    }

    fn fetch(&mut self) -> Result<String> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("failed to create {}", self.dir.display()))?;
        // atvremote always writes "artwork.png" into its working directory.
        let written = self.dir.join("artwork.png");
        let _ = std::fs::remove_file(&written);
        let status = Command::new(REMOTE_BIN)
            .args(&self.device_args)
            .arg("artwork_save")
            .current_dir(&self.dir)
            .stdout(Stdio::null())
            .status()
            .with_context(|| format!("failed to run {REMOTE_BIN}"))?;
        anyhow::ensure!(
            status.success(),
            "{REMOTE_BIN} artwork_save failed ({status}) — the device exposes \
             no artwork for this item"
        );
        ensure_usable(&written)?;
        // Publish under a fresh name every time: the drawing code decides what
        // to redraw by comparing URLs, so a single reused filename would make
        // every track look like the one already on screen.
        let path = self.dir.join(format!("art-{}.png", self.seq));
        std::fs::rename(&written, &path)
            .with_context(|| format!("failed to move artwork to {}", path.display()))?;
        if let Some(stale) = self.seq.checked_sub(KEEP_ARTWORK_FILES) {
            let _ = std::fs::remove_file(self.dir.join(format!("art-{stale}.png")));
        }
        self.seq += 1;
        Url::from_file_path(&path)
            .map(String::from)
            .map_err(|()| anyhow::anyhow!("artwork path is not a valid URL: {}", path.display()))
    }
}

/// Rejects what `atvremote artwork_save` leaves behind when the device has no
/// artwork to give: it can exit successfully having written an empty file,
/// and publishing that as a picture makes the drawing code retry a file it
/// can never decode.
fn ensure_usable(path: &std::path::Path) -> Result<()> {
    let size = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    anyhow::ensure!(
        size > 0,
        "{REMOTE_BIN} artwork_save produced no image — the device exposes \
         no artwork for this item"
    );
    Ok(())
}

/// Streams the HomePod's state onto `tx` until the process ends, then keeps
/// reconnecting — same strategy as the playerctl reader, because a HomePod
/// that goes to sleep also ends the subprocess.
pub fn spawn_reader(cfg: HomePod, tx: Sender<(SourceId, Option<NowPlaying>)>) -> Result<()> {
    for bin in [SCRIPT_BIN, REMOTE_BIN] {
        Command::new(bin)
            .arg("--help")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| {
                format!("{bin} not found in PATH — install pyatv (`uv tool install pyatv`)")
            })?;
    }
    let args = device_args(&cfg);
    std::thread::spawn(move || {
        let mut artwork = ArtworkCache::new(cache_dir(), args.clone());
        loop {
            follow(&args, &mut artwork, &tx);
            artwork.reset();
            // The device is gone as far as we know; say so before backing off.
            if tx.send((SourceId::HomePod, None)).is_err() {
                return;
            }
            std::thread::sleep(RECONNECT_DELAY);
        }
    });
    Ok(())
}

/// Runs one `atvscript push_updates` to completion. Returns once the child
/// exits, or immediately if the channel is gone.
fn follow(
    args: &[String],
    artwork: &mut ArtworkCache,
    tx: &Sender<(SourceId, Option<NowPlaying>)>,
) {
    // stdin stays piped and unused: atvscript keeps running until its stdin
    // closes, so handing it a null stdin would make it exit at once.
    let mut child = match Command::new(SCRIPT_BIN)
        .args(args)
        .arg("push_updates")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            eprintln!("failed to spawn {SCRIPT_BIN}: {err}");
            return;
        }
    };
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let line = match line {
                Ok(line) => line,
                Err(err) => {
                    eprintln!("{SCRIPT_BIN} read error: {err}");
                    break;
                }
            };
            let Some(update) = parse_line(&line) else {
                continue;
            };
            let now_playing = match update {
                Update::Gone => None,
                Update::State {
                    status: PlayStatus::Playing,
                    hash,
                } => Some(NowPlaying {
                    status: PlayStatus::Playing,
                    art_url: hash.and_then(|hash| artwork.url_for(&hash)),
                }),
                Update::State { status, .. } => Some(NowPlaying {
                    status,
                    art_url: None,
                }),
            };
            if tx.send((SourceId::HomePod, now_playing)).is_err() {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
        // Kill before wait: on a read error the child may still be alive, and
        // waiting on a live push_updates blocks forever.
        let _ = child.kill();
    }
    let _ = child.wait();
    eprintln!("{SCRIPT_BIN} exited; reconnecting in {RECONNECT_DELAY:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(line: &str) -> Update {
        parse_line(line).unwrap()
    }

    #[test]
    fn parses_a_playing_update_with_its_item_hash() {
        let update = state(
            r#"{"result": "success", "hash": "b0GMDX7O1::O86hBeXwz",
                "device_state": "playing", "title": "on the way to YOU"}"#,
        );
        assert_eq!(
            update,
            Update::State {
                status: PlayStatus::Playing,
                hash: Some("b0GMDX7O1::O86hBeXwz".to_string()),
            }
        );
    }

    #[test]
    fn maps_transient_states_to_playing() {
        for device_state in ["playing", "seeking", "loading"] {
            let line = format!(r#"{{"result": "success", "device_state": "{device_state}"}}"#);
            assert!(
                matches!(
                    state(&line),
                    Update::State {
                        status: PlayStatus::Playing,
                        ..
                    }
                ),
                "{device_state}"
            );
        }
    }

    #[test]
    fn maps_idle_and_stopped_to_stopped() {
        for device_state in ["idle", "stopped"] {
            let line = format!(r#"{{"result": "success", "device_state": "{device_state}"}}"#);
            assert!(
                matches!(
                    state(&line),
                    Update::State {
                        status: PlayStatus::Stopped,
                        ..
                    }
                ),
                "{device_state}"
            );
        }
    }

    #[test]
    fn parses_paused_without_a_hash() {
        assert_eq!(
            state(r#"{"result": "success", "device_state": "paused"}"#),
            Update::State {
                status: PlayStatus::Paused,
                hash: None,
            }
        );
    }

    #[test]
    fn a_failed_result_means_the_device_is_gone() {
        assert_eq!(
            state(r#"{"result": "failure", "exception": "connection lost"}"#),
            Update::Gone
        );
    }

    #[test]
    fn ignores_lines_that_say_nothing_about_playback() {
        assert_eq!(
            parse_line(r#"{"result": "success", "power_state": "on"}"#),
            None
        );
        assert_eq!(
            parse_line(r#"{"result": "success", "output_devices": []}"#),
            None
        );
    }

    #[test]
    fn ignores_unknown_device_states_and_garbage() {
        assert_eq!(
            parse_line(r#"{"result": "success", "device_state": "dancing"}"#),
            None
        );
        assert_eq!(parse_line("not json"), None);
        assert_eq!(parse_line(""), None);
    }

    fn cache() -> ArtworkCache {
        ArtworkCache::new(PathBuf::from("/nonexistent"), Vec::new())
    }

    #[test]
    fn an_item_is_only_asked_about_once() {
        let mut cache = cache();
        let asks = std::cell::Cell::new(0);
        let url = |cache: &mut ArtworkCache, hash: &str, answer: &str| {
            let answer = answer.to_string();
            cache.url_for_with(hash, |_| {
                asks.set(asks.get() + 1);
                Ok(answer)
            })
        };
        assert_eq!(
            url(&mut cache, "one", "file:///a.png").as_deref(),
            Some("file:///a.png")
        );
        // The device pushes the same item again while it plays.
        assert_eq!(
            url(&mut cache, "one", "file:///b.png").as_deref(),
            Some("file:///a.png")
        );
        assert_eq!(asks.get(), 1);
        assert_eq!(
            url(&mut cache, "two", "file:///c.png").as_deref(),
            Some("file:///c.png")
        );
        assert_eq!(asks.get(), 2);
    }

    #[test]
    fn an_item_with_no_artwork_is_not_asked_about_again() {
        // The expensive case: an AirPlay stream with no cover art pushes the
        // same item for the length of the track.
        let mut cache = cache();
        let asks = std::cell::Cell::new(0);
        let miss = |cache: &mut ArtworkCache| {
            cache.url_for_with("silent", |_| {
                asks.set(asks.get() + 1);
                anyhow::bail!("no artwork")
            })
        };
        assert_eq!(miss(&mut cache), None);
        assert_eq!(miss(&mut cache), None);
        assert_eq!(asks.get(), 1);
    }

    #[test]
    fn a_reconnect_makes_the_next_ask_real_again() {
        // A miss caused by the connection dropping must not outlive it.
        let mut cache = cache();
        let asks = std::cell::Cell::new(0);
        assert_eq!(
            cache.url_for_with("one", |_| {
                asks.set(asks.get() + 1);
                anyhow::bail!("connection lost")
            }),
            None
        );
        cache.reset();
        assert_eq!(
            cache
                .url_for_with("one", |_| {
                    asks.set(asks.get() + 1);
                    Ok("file:///a.png".to_string())
                })
                .as_deref(),
            Some("file:///a.png")
        );
        assert_eq!(asks.get(), 2);
    }

    #[test]
    fn an_empty_artwork_file_is_not_artwork() {
        // atvremote exits 0 after writing 0 bytes when the HomePod is playing
        // an AirPlay stream from a third-party app (pyatv#2891); publishing
        // that file left the daemon retrying an undecodable image forever.
        let dir = std::env::temp_dir().join("pixoo-nowplaying-homepod-test");
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("empty.png");
        std::fs::write(&empty, b"").unwrap();
        assert!(ensure_usable(&empty).is_err());
        let real = dir.join("real.png");
        std::fs::write(&real, b"\x89PNG").unwrap();
        assert!(ensure_usable(&real).is_ok());
        assert!(ensure_usable(&dir.join("missing.png")).is_err());
        std::fs::remove_file(&empty).unwrap();
        std::fs::remove_file(&real).unwrap();
    }

    #[test]
    fn device_args_address_the_identifier_over_airplay() {
        let cfg = HomePod {
            identifier: "46:B5:0E:8C:4A:17".to_string(),
            address: None,
        };
        assert_eq!(
            device_args(&cfg),
            vec!["--id", "46:B5:0E:8C:4A:17", "--protocol", "airplay"]
        );
    }

    #[test]
    fn a_configured_address_skips_discovery() {
        let cfg = HomePod {
            identifier: "46:B5:0E:8C:4A:17".to_string(),
            address: Some("192.168.0.141".to_string()),
        };
        assert_eq!(
            device_args(&cfg),
            vec![
                "--id",
                "46:B5:0E:8C:4A:17",
                "--protocol",
                "airplay",
                "--scan-hosts",
                "192.168.0.141",
            ]
        );
    }
}
