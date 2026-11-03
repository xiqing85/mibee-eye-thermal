//! Live thermal stream hub: sensor poller → To frames → openh264 access
//! units → GB28181 FrameSource fan-out.
//!
//! Two producers keep the stream alive under any hardware condition:
//! - the **sensor thread** reads EE once, then loops frame+aux reads through
//!   the M2 compensation chain (bit-bang today ~40 s/frame, hardware TWI
//!   after the pull-up fix);
//! - the **encoder thread** always ticks at the configured fps over the
//!   *latest* To grid — when no real frame has landed yet (or the sensor is
//!   down) it renders a smooth synthetic field, honestly marked in
//!   `ThermalStatus.source`, so NVR-side pre-integration ("占位伪彩流")
//!   runs without any hardware dependency.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use gb28181_rs::frame::{AccessUnit, FrameSource, FrameSubscription, Nalu};
use openh264::encoder::{Encoder, EncoderConfig};
use std::sync::mpsc;

use crate::ee::{CommonParams, Eeprom};
use crate::i2c::{bitbang::BitBang, twi::Twi, MlxBus};
use crate::mlx90640::{AuxReadings, Mlx90640, HEIGHT, PIXELS, WIDTH};
use crate::render::{render_rgb, rgb_to_i420};
use crate::settings::{BusSpec, StreamConfig};

/// What the currently served thermal frame is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOrigin {
    /// Real sensor data through the full M2 compensation chain.
    Sensor,
    /// Animated synthetic placeholder (pre-integration / sensor down).
    Synthetic,
}

/// Shared snapshot of the latest thermal frame + health, read by the HTTP
/// API and written by the sensor thread.
#[derive(Debug, Clone)]
pub struct ThermalStatus {
    pub origin: FrameOrigin,
    pub frame_number: u64,
    pub timestamp_ms: u64,
    pub to: [f32; PIXELS],
    /// Present when the last sensor pass succeeded.
    pub common: Option<CommonParams>,
    pub last_sensor_error: Option<String>,
    /// Sensor frames per second actually achieved (bit-bang ≈ 0.03).
    pub sensor_fps: f32,
}

impl ThermalStatus {
    pub(crate) fn synthetic(t: f64, frame_number: u64) -> ThermalStatus {
        let mut to = [0f32; PIXELS];
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let fx = x as f64 / WIDTH as f64;
                let fy = y as f64 / HEIGHT as f64;
                to[y * WIDTH + x] = (25.0
                    + 6.0 * (fx * 6.0 + t * 0.9).cos()
                    + 5.0 * (fy * 5.0 - t * 0.6).sin()
                    + 3.0 * (fx * fy * 4.0 * std::f64::consts::PI).sin())
                    as f32;
            }
        }
        ThermalStatus {
            origin: FrameOrigin::Synthetic,
            frame_number,
            timestamp_ms: now_ms(),
            to,
            common: None,
            last_sensor_error: None,
            sensor_fps: 0.0,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// gb28181-rs live-frame source over a bounded fan-out registry.
pub struct ThermalHub {
    subs: Arc<Mutex<HashMap<u64, mpsc::SyncSender<AccessUnit>>>>,
    next_id: AtomicU64,
    stop: Arc<AtomicBool>,
    status: Arc<RwLock<ThermalStatus>>,
    idr_flag: Arc<AtomicBool>,
    /// Diagnostics for /healthz.
    pub encoded_frames: Arc<AtomicU64>,
}

impl ThermalHub {
    pub fn status(&self) -> ThermalStatus {
        self.status.read().expect("thermal status lock").clone()
    }

    pub fn request_idr(&self) {
        self.idr_flag.store(true, Ordering::Relaxed);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Spawn the sensor + encoder threads with an explicit sensor address
    /// (A1/A0 straps move MLX90640 across 0x30..=0x3E; `scan` finds the
    /// live one). The hub itself is inert data — production wiring happens
    /// here.
    pub fn start_with_addr(cfg: &StreamConfig, bus: &BusSpec, addr: u8) -> Arc<ThermalHub> {
        let hub = Arc::new(ThermalHub {
            subs: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            stop: Arc::new(AtomicBool::new(false)),
            status: Arc::new(RwLock::new(ThermalStatus::synthetic(0.0, 0))),
            idr_flag: Arc::new(AtomicBool::new(true)),
            encoded_frames: Arc::new(AtomicU64::new(0)),
        });

        // Sensor thread — real frames replace the synthetic placeholder.
        {
            let hub = Arc::clone(&hub);
            let bus = bus.clone();
            let emissivity = cfg.emissivity;
            thread::Builder::new()
                .name("mlx-sensor".into())
                .spawn(move || sensor_loop(hub, bus, addr, emissivity))
                .expect("spawn sensor thread");
        }

        // Encoder thread — fixed fps over the latest frame.
        {
            let hub = Arc::clone(&hub);
            let (w, h, fps) = (cfg.width, cfg.height, cfg.fps);
            let bitrate = cfg.bitrate_bps;
            thread::Builder::new()
                .name("openh264-encoder".into())
                .spawn(move || encoder_loop(hub, w, h, fps, bitrate))
                .expect("spawn encoder thread");
        }

        hub
    }
}

impl FrameSource for ThermalHub {
    fn subscribe_with_capacity(&self, capacity: usize) -> FrameSubscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Bounded sync channel: slow subscribers get frames dropped via
        // try_send instead of growing a queue (gb28181-rs expects std mpsc).
        let (tx, rx) = mpsc::sync_channel(capacity.max(1));
        self.subs.lock().expect("subs lock").insert(id, tx);
        // Fresh subscriber → next encoded frame must be decodable standalone.
        self.idr_flag.store(true, Ordering::Relaxed);
        FrameSubscription { id, receiver: rx }
    }

    fn unsubscribe(&self, id: u64) {
        self.subs.lock().expect("subs lock").remove(&id);
    }
}

/// Release the TWI2 driver + stale sysfs exports so bit-bang can own PH2/PH3
/// (same dance as the bring-up viewer; idempotent, best-effort).
fn release_i2c_pins(sda: u32, scl: u32) {
    let _ = std::fs::write(
        "/sys/bus/platform/drivers/mv64xxx_i2c/unbind",
        "5002800.i2c",
    );
    for n in [sda, scl] {
        let _ = std::fs::write("/sys/class/gpio/unexport", n.to_string());
    }
    std::thread::sleep(Duration::from_millis(300));
}

enum AnyBus {
    Twi(Twi),
    Gpio(BitBang),
}

impl MlxBus for AnyBus {
    fn read_reg16(&mut self, reg: u16) -> crate::Result<u16> {
        match self {
            AnyBus::Twi(b) => b.read_reg16(reg),
            AnyBus::Gpio(b) => b.read_reg16(reg),
        }
    }
    fn read_block(&mut self, reg: u16, buf: &mut [u8]) -> crate::Result<()> {
        match self {
            AnyBus::Twi(b) => b.read_block(reg, buf),
            AnyBus::Gpio(b) => b.read_block(reg, buf),
        }
    }
    #[allow(dead_code)]
    fn write_reg16(&mut self, reg: u16, value: u16) -> crate::Result<()> {
        match self {
            AnyBus::Twi(b) => b.write_reg16(reg, value),
            AnyBus::Gpio(b) => b.write_reg16(reg, value),
        }
    }
}

fn open_bus(spec: &BusSpec, addr: u8) -> crate::Result<AnyBus> {
    match spec {
        BusSpec::Twi(dev) => Ok(AnyBus::Twi(Twi::open(dev, addr)?)),
        BusSpec::Gpio { sda, scl } => {
            release_i2c_pins(*sda, *scl);
            Ok(AnyBus::Gpio(BitBang::new(*sda, *scl, addr)?))
        }
        BusSpec::Auto => {
            // Boot/hot-plug discovery: hardware TWI first, bit-bang fallback.
            // The detected address is the authoritative one (something
            // answered at it); the configured `addr` is ignored on this path.
            match crate::scan::auto_detect() {
                Some((hit, false)) => {
                    let dev = hit.bus.trim_start_matches("twi:").to_string();
                    Ok(AnyBus::Twi(Twi::open(&dev, hit.addr)?))
                }
                Some((hit, true)) => {
                    let pins = hit.bus.trim_start_matches("gpio:");
                    let (sda, scl) = pins.split_once(',').ok_or_else(|| {
                        crate::error::Error::Bus("auto-detect returned bad pair".into())
                    })?;
                    let (sda, scl): (u32, u32) = (
                        sda.parse().map_err(|_| {
                            crate::error::Error::Bus("auto-detect returned bad pin".into())
                        })?,
                        scl.parse().map_err(|_| {
                            crate::error::Error::Bus("auto-detect returned bad pin".into())
                        })?,
                    );
                    release_i2c_pins(sda, scl);
                    Ok(AnyBus::Gpio(BitBang::new(sda, scl, hit.addr)?))
                }
                None => Err(crate::error::Error::Bus(
                    "auto-detect found no MLX90640 (see `mibee-eye-thermal scan`)".into(),
                )),
            }
        }
    }
}

fn sensor_loop(hub: Arc<ThermalHub>, bus: BusSpec, addr: u8, emissivity: f64) {
    // First synthetic frame already in place; keep producing real ones.
    let mut eeprom: Option<Eeprom> = None;
    let mut failures = 0u32;
    while !hub.stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let pass = (|| -> Option<([f32; PIXELS], CommonParams)> {
            let bus = match open_bus(&bus, addr) {
                Ok(b) => b,
                Err(e) => {
                    hub.status.write().expect("status lock").last_sensor_error =
                        Some(format!("bus open: {e}"));
                    return None;
                }
            };
            let mut sensor = Mlx90640::new(bus);
            let ee = match &eeprom {
                Some(ee) => ee.clone(),
                None => {
                    let words = sensor.read_eeprom().ok()?;
                    let ee = Eeprom::from_words(&words);
                    eeprom = Some(ee.clone());
                    ee
                }
            };
            let aux: AuxReadings = sensor.read_aux().ok()?;
            let frame = sensor.read_frame_raw().ok()?;
            let (to, common) = ee.compute_to(&frame, &aux, emissivity, None);
            Some((to, common))
        })();
        match pass {
            Some((to, common)) => {
                failures = 0;
                let mut st = hub.status.write().expect("status lock");
                st.sensor_fps = 1.0f32 / started.elapsed().as_secs_f32().max(1.0 / 32.0);
                let frame_number = st.frame_number.wrapping_add(1);
                if frame_number == 1 {
                    eprintln!("sensor: first real frame landed (ta={:.2})", common.ta);
                }
                *st = ThermalStatus {
                    origin: FrameOrigin::Sensor,
                    frame_number,
                    timestamp_ms: now_ms(),
                    to,
                    common: Some(common),
                    // keep last error visible even after recovery
                    last_sensor_error: st.last_sensor_error.clone(),
                    sensor_fps: st.sensor_fps,
                };
            }
            None => {
                failures += 1;
                let mut st = hub.status.write().expect("status lock");
                st.sensor_fps = 0.0;
                st.last_sensor_error = Some(if failures == 1 {
                    "sensor read failed (bit-bang bus or wiring)".into()
                } else {
                    format!("sensor read failed x{failures} (bit-bang bus or wiring)")
                });
                eprintln!(
                    "warning: sensor pass failed (x{failures}), last error: {:?}",
                    st.last_sensor_error
                );
            }
        }
        // Bit-bang pass itself takes tens of seconds; don't hammer the bus
        // on failure either. Hardware TWI lands with the pull-up fix.
        let backoff = if failures == 0 {
            1u64
        } else {
            u64::from(failures.min(30))
        };
        thread::sleep(Duration::from_secs(backoff));
    }
}

/// Split an Annex-B H.264 byte stream into header-less NALUs.
fn split_annexb(data: &[u8]) -> Vec<Nalu> {
    let mut nalus = Vec::new();
    let mut starts: Vec<usize> = Vec::new(); // position of the NAL payload
    let mut i = 0usize;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else if i + 4 <= data.len() && data[i..i + 4] == [0, 0, 0, 1] {
            starts.push(i + 4);
            i += 4;
        } else {
            i += 1;
        }
    }
    for (k, &s) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(data.len());
        // end points at the next payload start — back out its start code.
        let end = if k + 1 < starts.len() {
            let mut e = end;
            if e >= 4 && data[e - 4..e] == [0, 0, 0, 1] {
                e -= 4;
            } else if e >= 3 && data[e - 3..e] == [0, 0, 1] {
                e -= 3;
            }
            e
        } else {
            end
        };
        if end <= s || s >= data.len() {
            continue;
        }
        let body = &data[s..end];
        let nalu_type = body[0] & 0x1F;
        nalus.push(Nalu {
            nalu_type,
            data: body.to_vec(),
            is_idr: nalu_type == 5,
            is_sps: nalu_type == 7,
            is_pps: nalu_type == 8,
            is_aud: nalu_type == 9,
        });
    }
    nalus
}

fn encoder_loop(hub: Arc<ThermalHub>, width: usize, height: usize, fps: f32, bitrate_bps: u32) {
    // Dimensions travel per-frame (YUVSource::dimensions) in openh264 0.9.x.
    let config = EncoderConfig::new()
        .max_frame_rate(openh264::encoder::FrameRate::from_hz(fps))
        .bitrate(openh264::encoder::BitRate::from_bps(bitrate_bps))
        .rate_control_mode(openh264::encoder::RateControlMode::Bitrate)
        .intra_frame_period(openh264::encoder::IntraFramePeriod::from_num_frames(8));
    let mut encoder = match Encoder::with_api_config(openh264::OpenH264API::from_source(), config) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: openh264 init failed: {e}");
            return;
        }
    };
    let tick = Duration::from_secs_f32(1.0f32 / fps.max(0.1));
    let mut t_synthetic = 0f64;
    loop {
        let frame_start = Instant::now();
        if hub.stop.load(Ordering::Relaxed) {
            break;
        }
        let mut status = hub.status.read().expect("status lock").clone();
        if status.origin == FrameOrigin::Synthetic {
            // Keep the placeholder alive: animate the synthetic field.
            t_synthetic += 1.0 / fps.max(0.1) as f64;
            status = ThermalStatus::synthetic(t_synthetic, status.frame_number);
        }
        let rgb = render_rgb(&status.to, width, height);
        let i420 = rgb_to_i420(&rgb, width, height);

        if hub.idr_flag.swap(false, Ordering::Relaxed) {
            encoder.force_intra_frame();
        }
        let view = openh264::formats::YUVBuffer::from_vec(i420, width, height);
        match encoder.encode(&view) {
            Ok(bitstream) => {
                let mut data = Vec::with_capacity(8192);
                bitstream.write_vec(&mut data);
                let nalus = split_annexb(&data);
                let key = nalus.iter().any(|n| n.is_idr);
                let au = AccessUnit {
                    nalus,
                    timestamp: Instant::now(),
                    is_key_frame: key,
                };
                hub.encoded_frames.fetch_add(1, Ordering::Relaxed);
                let subs = hub.subs.lock().expect("subs lock");
                for tx in subs.values() {
                    // Bounded: drop for slow subscribers, never block.
                    let _ = tx.try_send(au.clone());
                }
            }
            Err(e) => eprintln!("warning: openh264 encode failed: {e}"),
        }
        let elapsed = frame_start.elapsed();
        if elapsed < tick {
            thread::sleep(tick - elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_splitter_handles_sps_pps_idr() {
        // SPS | PPS | IDR, mixed 4-byte and 3-byte start codes.
        let mut data = vec![0, 0, 0, 1];
        data.extend_from_slice(&[0x67, 0x42, 0x00, 0x1F]); // SPS
        data.extend_from_slice(&[0, 0, 1]);
        data.extend_from_slice(&[0x68, 0xCE]); // PPS
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0x65, 0x88, 0x84, 0x21, 0xA0]); // IDR
        let nalus = split_annexb(&data);
        assert_eq!(nalus.len(), 3);
        assert!(nalus[0].is_sps);
        assert!(nalus[1].is_pps);
        assert!(nalus[2].is_idr);
        assert_eq!(nalus[2].data, vec![0x65, 0x88, 0x84, 0x21, 0xA0]);
    }

    #[test]
    fn synthetic_frame_stays_in_plausible_band() {
        let st = ThermalStatus::synthetic(3.7, 42);
        assert_eq!(st.origin, FrameOrigin::Synthetic);
        assert_eq!(st.frame_number, 42);
        assert!(st.to.iter().all(|v| *v > 5.0 && *v < 45.0));
    }

    #[test]
    fn subscribe_unsubscribe_bookkeeping() {
        let hub = ThermalHub {
            subs: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU64::new(1),
            stop: Arc::new(AtomicBool::new(false)),
            status: Arc::new(RwLock::new(ThermalStatus::synthetic(0.0, 0))),
            idr_flag: Arc::new(AtomicBool::new(false)),
            encoded_frames: Arc::new(AtomicU64::new(0)),
        };
        let sub = <ThermalHub as FrameSource>::subscribe_with_capacity(&hub, 2);
        assert!(hub.idr_flag.load(Ordering::Relaxed));
        assert!(hub.subs.lock().unwrap().contains_key(&sub.id));
        <ThermalHub as FrameSource>::unsubscribe(&hub, sub.id);
        assert!(!hub.subs.lock().unwrap().contains_key(&sub.id));
    }
}
