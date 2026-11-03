//! Service configuration (TOML) — see `config.example.toml` for the
//! annotated reference. CLI bring-up commands (`id`/`frame`/`ta`) don't
//! need this; only `serve` does.

use gb28181_rs::config::Gb28181Config;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BusSpec {
    Twi(String),
    Gpio {
        sda: u32,
        scl: u32,
    },
    /// Boot-time discovery: hardware TWI buses first, then the classic
    /// bit-bang fallback pair (226/227 on the Orange Pi Zero 3).
    Auto,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SensorConfig {
    #[serde(default = "default_bus")]
    pub bus: BusSpec,
    /// MLX90640 7-bit address (factory 0x33; A1/A0 straps move it in
    /// 0x30..=0x3E — run `scan` when in doubt).
    #[serde(default = "default_addr")]
    pub addr: u8,
    /// Re-read the EEPROM every N sensor passes (0 = once at boot).
    /// Reserved for the hot-swap/debug path; the sensor loop reads EE once
    /// today and this knob is honored when EEPROM refresh lands.
    #[serde(default)]
    #[allow(dead_code)]
    pub eeprom_refresh_passes: u32,
}

fn default_bus() -> BusSpec {
    BusSpec::Gpio { sda: 226, scl: 227 }
}
fn default_addr() -> u8 {
    0x33
}

impl Default for SensorConfig {
    fn default() -> Self {
        Self {
            bus: default_bus(),
            addr: default_addr(),
            eeprom_refresh_passes: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StreamConfig {
    #[serde(default = "default_width")]
    pub width: usize,
    #[serde(default = "default_height")]
    pub height: usize,
    #[serde(default = "default_fps")]
    pub fps: f32,
    #[serde(default = "default_bitrate")]
    pub bitrate_bps: u32,
    /// Emissivity for the To chain (user calibration, 0 < ε ≤ 1).
    #[serde(default = "default_emissivity")]
    pub emissivity: f64,
}

fn default_width() -> usize {
    320
}
fn default_height() -> usize {
    240
}
fn default_fps() -> f32 {
    2.0
}
fn default_bitrate() -> u32 {
    512_000
}
fn default_emissivity() -> f64 {
    1.0
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
            fps: default_fps(),
            bitrate_bps: default_bitrate(),
            emissivity: default_emissivity(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpConfig {
    #[serde(default = "default_http_enabled")]
    pub enabled: bool,
    #[serde(default = "default_http_port")]
    pub port: u16,
    /// Admin password for the family SPEC session envelope. Empty (default)
    /// = open bring-up mode (no login; LAN only). Constrained ASCII — the
    /// mini JSON parser on the login path does not unescape quotes.
    #[serde(default)]
    pub password: String,
    /// Static bearer token for machine consumers (`Authorization: Bearer
    /// <token>`); empty = off. Either this or a session cookie passes the
    /// /api/* gate when a password is set.
    #[serde(default)]
    pub api_token: String,
    /// Family webui bundle directory (`make sync-thermal` destination).
    /// Falls back to the built-in viewer page when absent.
    #[serde(default = "default_static_dir")]
    pub static_dir: String,
}

fn default_http_enabled() -> bool {
    true
}
fn default_http_port() -> u16 {
    8098
}
fn default_static_dir() -> String {
    "/usr/local/share/mibee-eye-thermal/static".into()
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            enabled: default_http_enabled(),
            port: default_http_port(),
            password: String::new(),
            api_token: String::new(),
            static_dir: default_static_dir(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub sensor: SensorConfig,
    #[serde(default)]
    pub stream: StreamConfig,
    #[serde(default)]
    pub http: HttpConfig,
    #[serde(default)]
    pub gb28181: Gb28181Config,
}

impl Settings {
    pub fn load_toml(text: &str) -> Result<Self, String> {
        toml::from_str(text).map_err(|e| format!("config parse: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_fills_defaults() {
        let s = Settings::load_toml("").expect("empty config defaults");
        assert_eq!(s.stream.width, 320);
        assert_eq!(s.stream.height, 240);
        assert_eq!(s.stream.fps, 2.0);
        assert!(!s.gb28181.enabled);
        assert!(matches!(s.sensor.bus, BusSpec::Gpio { sda: 226, scl: 227 }));
    }

    #[test]
    fn thermal_channel_marking_roundtrip() {
        let text = r#"
[gb28181]
enabled = true
platform_sip_address = "192.168.63.30"
device_id = "34020000001320000003"
channel_id = "34020000001320000003"
sip_domain = "3402000000"
photoelectric_imaging_type = 2

[stream]
fps = 4.0
"#;
        let s = Settings::load_toml(text).expect("parse");
        assert!(s.gb28181.enabled);
        assert_eq!(s.gb28181.photoelectric_imaging_type, Some(2));
        assert_eq!(s.gb28181.platform_sip_address, "192.168.63.30");
        assert_eq!(s.stream.fps, 4.0);
        // Bus default still applies.
        assert!(matches!(s.sensor.bus, BusSpec::Gpio { .. }));
    }
}
