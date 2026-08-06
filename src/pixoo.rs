use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use serde_json::{json, Value};

pub const SIZE: u32 = 64;
const FRAME_BYTES: usize = (SIZE * SIZE * 3) as usize;

pub fn reset_gif_payload() -> Value {
    json!({ "Command": "Draw/ResetHttpGifId" })
}

/// Builds a single-frame Draw/SendHttpGif payload. `rgb` is raw RGB888,
/// 64x64x3 bytes — despite the command name this is not a GIF file.
pub fn frame_payload(rgb: &[u8]) -> Result<Value> {
    if rgb.len() != FRAME_BYTES {
        bail!("expected {FRAME_BYTES} RGB bytes, got {}", rgb.len());
    }
    Ok(json!({
        "Command": "Draw/SendHttpGif",
        "PicNum": 1,
        "PicWidth": SIZE,
        "PicOffset": 0,
        "PicID": 1,
        "PicSpeed": 1000,
        "PicData": BASE64.encode(rgb),
    }))
}

pub fn set_channel_payload(index: u8) -> Value {
    json!({ "Command": "Channel/SetIndex", "SelectIndex": index })
}

pub struct Client {
    endpoint: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(ip: &str) -> Self {
        Self {
            endpoint: format!("http://{ip}/post"),
            // The Pixoo's embedded HTTP server handles kept-alive connections
            // poorly across long idle gaps, so pooling is disabled.
            agent: ureq::AgentBuilder::new()
                .timeout(std::time::Duration::from_secs(10))
                .max_idle_connections(0)
                .build(),
        }
    }

    fn post(&self, payload: &Value) -> Result<()> {
        let body: Value = self
            .agent
            .post(&self.endpoint)
            .send_json(payload)
            .with_context(|| format!("POST {} failed", self.endpoint))?
            .into_json()
            .context("Pixoo returned non-JSON response")?;
        match body.get("error_code").and_then(Value::as_i64) {
            Some(0) => Ok(()),
            code => bail!("Pixoo rejected {}: error_code={code:?}", payload["Command"]),
        }
    }

    pub fn show_frame(&self, rgb: &[u8]) -> Result<()> {
        self.post(&reset_gif_payload())?;
        self.post(&frame_payload(rgb)?)
    }

    pub fn set_channel(&self, index: u8) -> Result<()> {
        self.post(&set_channel_payload(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_payload_encodes_rgb_as_base64() {
        let rgb = vec![0u8; FRAME_BYTES];
        let payload = frame_payload(&rgb).unwrap();
        assert_eq!(payload["Command"], "Draw/SendHttpGif");
        assert_eq!(payload["PicWidth"], 64);
        assert_eq!(payload["PicNum"], 1);
        let data = payload["PicData"].as_str().unwrap();
        assert_eq!(BASE64.decode(data).unwrap().len(), FRAME_BYTES);
    }

    #[test]
    fn frame_payload_rejects_wrong_size() {
        assert!(frame_payload(&[0u8; 100]).is_err());
    }

    #[test]
    fn set_channel_payload_shape() {
        let payload = set_channel_payload(1);
        assert_eq!(payload["Command"], "Channel/SetIndex");
        assert_eq!(payload["SelectIndex"], 1);
    }

    #[test]
    fn reset_payload_shape() {
        assert_eq!(reset_gif_payload()["Command"], "Draw/ResetHttpGifId");
    }
}
