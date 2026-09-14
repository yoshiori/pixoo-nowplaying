use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_IDLE_RESTORE_SECS: u64 = 30;

#[derive(Debug, Deserialize, PartialEq)]
pub struct Config {
    pub pixoo_ip: String,
    /// When unset, the daemon restores to whatever channel the device
    /// reported just before artwork took the screen over.
    #[serde(default)]
    pub restore_channel: Option<u8>,
    #[serde(default = "default_idle_restore_secs")]
    pub idle_restore_secs: u64,
    /// Players the daemon never follows, passed to playerctl as
    /// `--ignore-player`. Names as reported by `playerctl -l`
    /// (a base name also covers its `.instanceNNN` variants).
    #[serde(default)]
    pub excluded_players: Vec<String>,
    /// When set, a HomePod on the LAN is followed alongside local MPRIS.
    #[serde(default)]
    pub homepod: Option<HomePod>,
}

/// A HomePod to follow through pyatv.
#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct HomePod {
    /// Device identifier as reported by `atvremote scan`.
    pub identifier: String,
    /// The device's IP. Optional: it only saves pyatv the mDNS discovery,
    /// which is worth doing on hosts with many interfaces (VPNs, bridges).
    #[serde(default)]
    pub address: Option<String>,
}

fn default_idle_restore_secs() -> u64 {
    DEFAULT_IDLE_RESTORE_SECS
}

pub fn parse(s: &str) -> Result<Config> {
    let mut config: Config = toml::from_str(s).context("invalid config")?;
    if let Some(channel) = config.restore_channel {
        anyhow::ensure!(
            channel < crate::pixoo::CHANNEL_COUNT,
            "restore_channel must be 0-{} (got {channel})",
            crate::pixoo::CHANNEL_COUNT - 1
        );
    }
    // Entries become a comma-separated playerctl --ignore-player argument,
    // so a comma or embedded whitespace would silently change what is
    // ignored. Fail loudly instead.
    for entry in &mut config.excluded_players {
        *entry = entry.trim().to_string();
        anyhow::ensure!(
            !entry.is_empty() && !entry.contains([',', ' ', '\t']),
            "excluded_players entries must be non-empty player names \
             without commas or whitespace (got {entry:?})"
        );
    }
    if let Some(homepod) = &mut config.homepod {
        homepod.identifier = homepod.identifier.trim().to_string();
        anyhow::ensure!(
            !homepod.identifier.is_empty(),
            "homepod.identifier must be a device identifier from `atvremote scan`"
        );
        if let Some(address) = &mut homepod.address {
            *address = address.trim().to_string();
            anyhow::ensure!(!address.is_empty(), "homepod.address must not be empty");
        }
    }
    Ok(config)
}

pub fn config_path() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(base.join("pixoo-nowplaying/config.toml"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config at {}", path.display()))?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config_with_defaults() {
        let c = parse(r#"pixoo_ip = "192.168.0.153""#).unwrap();
        assert_eq!(c.pixoo_ip, "192.168.0.153");
        assert_eq!(c.restore_channel, None);
        assert_eq!(c.idle_restore_secs, 30);
        assert!(c.excluded_players.is_empty());
        assert_eq!(c.homepod, None);
    }

    #[test]
    fn parses_homepod_section() {
        let c = parse(
            r#"
            pixoo_ip = "10.0.0.5"

            [homepod]
            identifier = " 46:B5:0E:8C:4A:17 "
            address = "192.168.0.141"
            "#,
        )
        .unwrap();
        assert_eq!(
            c.homepod,
            Some(HomePod {
                identifier: "46:B5:0E:8C:4A:17".to_string(),
                address: Some("192.168.0.141".to_string()),
            })
        );
    }

    #[test]
    fn homepod_address_is_optional() {
        let c = parse("pixoo_ip = \"10.0.0.5\"\n[homepod]\nidentifier = \"abc\"").unwrap();
        assert_eq!(c.homepod.unwrap().address, None);
    }

    #[test]
    fn blank_homepod_identifier_is_an_error() {
        let err = parse("pixoo_ip = \"10.0.0.5\"\n[homepod]\nidentifier = \"  \"").unwrap_err();
        assert!(err.to_string().contains("homepod.identifier"));
    }

    #[test]
    fn blank_homepod_address_is_an_error() {
        let err = parse("pixoo_ip = \"10.0.0.5\"\n[homepod]\nidentifier = \"abc\"\naddress = \"\"")
            .unwrap_err();
        assert!(err.to_string().contains("homepod.address"));
    }

    #[test]
    fn parses_excluded_players_list() {
        let c = parse(
            r#"
            pixoo_ip = "10.0.0.5"
            excluded_players = ["chromium", "firefox"]
            "#,
        )
        .unwrap();
        assert_eq!(c.excluded_players, vec!["chromium", "firefox"]);
    }

    #[test]
    fn excluded_players_entries_are_trimmed() {
        let c = parse(
            r#"
            pixoo_ip = "10.0.0.5"
            excluded_players = [" chromium ", "firefox"]
            "#,
        )
        .unwrap();
        assert_eq!(c.excluded_players, vec!["chromium", "firefox"]);
    }

    #[test]
    fn empty_excluded_player_entry_is_an_error() {
        let err = parse("pixoo_ip = \"10.0.0.5\"\nexcluded_players = [\"\"]").unwrap_err();
        assert!(err.to_string().contains("excluded_players"));
    }

    #[test]
    fn excluded_player_entry_with_comma_or_space_is_an_error() {
        for entry in ["chromium,firefox", "chrom ium"] {
            let toml = format!("pixoo_ip = \"10.0.0.5\"\nexcluded_players = [\"{entry}\"]");
            let err = parse(&toml).unwrap_err();
            assert!(err.to_string().contains("excluded_players"), "{entry}");
        }
    }

    #[test]
    fn parses_full_config() {
        let c = parse(
            r#"
            pixoo_ip = "10.0.0.5"
            restore_channel = 0
            idle_restore_secs = 60
            "#,
        )
        .unwrap();
        assert_eq!(c.restore_channel, Some(0));
        assert_eq!(c.idle_restore_secs, 60);
    }

    #[test]
    fn missing_ip_is_an_error() {
        assert!(parse("restore_channel = 1").is_err());
    }

    #[test]
    fn out_of_range_restore_channel_is_an_error() {
        let err = parse("pixoo_ip = \"10.0.0.5\"\nrestore_channel = 255").unwrap_err();
        assert!(err.to_string().contains("restore_channel"));
    }
}
