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
}

fn default_idle_restore_secs() -> u64 {
    DEFAULT_IDLE_RESTORE_SECS
}

pub fn parse(s: &str) -> Result<Config> {
    let config: Config = toml::from_str(s).context("invalid config")?;
    if let Some(channel) = config.restore_channel {
        anyhow::ensure!(
            channel < crate::pixoo::CHANNEL_COUNT,
            "restore_channel must be 0-{} (got {channel})",
            crate::pixoo::CHANNEL_COUNT - 1
        );
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
