//! mibee-eye-thermal — MLX90640 thermal camera for the MiBee Eye family.
//!
//! Bring-up CLI (v0.0.x):
//!   mibee-eye-thermal id     --bus twi:/dev/i2c-2
//!   mibee-eye-thermal frame  --bus gpio:226,227
//!   mibee-eye-thermal ta     --bus gpio:226,227   (EE decode + Ta/To frame)
//! The service build (web/RTSP/GB28181) layers on top of the same driver.

mod ee;
mod error;
mod httpsrv;
mod hub;
mod i2c;
mod mlx90640;
mod render;
mod scan;
mod settings;

use error::Result;
use i2c::{bitbang::BitBang, twi::Twi};
use mlx90640::Mlx90640;

enum Bus {
    Twi(String),
    Gpio(u32, u32),
}

fn parse_args() -> Result<(String, Bus, Option<String>)> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args
        .get(1)
        .ok_or_else(|| {
            error::Error::Usage(
                "usage: mibee-eye-thermal <id|frame|ta|serve> [--bus <twi:/dev/i2c-N|gpio:SDA,SCL>] [--config <path>]"
                    .into(),
            )
        })?
        .clone();
    let mut bus = Bus::Twi("/dev/i2c-2".into());
    let mut config_path: Option<String> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--bus" => {
                i += 1;
                let spec = args
                    .get(i)
                    .ok_or_else(|| error::Error::Usage("--bus needs a value".into()))?;
                if let Some(dev) = spec.strip_prefix("twi:") {
                    bus = Bus::Twi(dev.into());
                } else if let Some(pins) = spec.strip_prefix("gpio:") {
                    let mut it = pins.split(',');
                    let sda: u32 = it.next().and_then(|v| v.parse().ok()).ok_or_else(|| {
                        error::Error::Usage("gpio bus: SDA,SCL numbers expected".into())
                    })?;
                    let scl: u32 = it.next().and_then(|v| v.parse().ok()).ok_or_else(|| {
                        error::Error::Usage("gpio bus: SDA,SCL numbers expected".into())
                    })?;
                    bus = Bus::Gpio(sda, scl);
                } else {
                    return Err(error::Error::Usage(format!("unknown bus spec: {spec}")));
                }
            }
            "--config" => {
                i += 1;
                config_path = Some(
                    args.get(i)
                        .ok_or_else(|| error::Error::Usage("--config needs a path".into()))?
                        .clone(),
                );
            }
            other => return Err(error::Error::Usage(format!("unknown arg: {other}"))),
        }
        i += 1;
    }
    Ok((cmd, bus, config_path))
}

/// Human-readable bus owner (sysfs name) — keeps scan output honest about
/// which controller each /dev/i2c-N is (PMIC, HDMI DDC, pin header, ...).
fn bus_owner(dev: &str) -> String {
    let n = dev.rsplit('-').next().unwrap_or("");
    std::fs::read_to_string(format!("/sys/bus/i2c/devices/i2c-{n}/name"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn run() -> Result<()> {
    let (cmd, bus, config_path) = parse_args()?;
    match cmd.as_str() {
        "id" => {
            let (id, status, control, ptat): (u16, u16, u16, i16);
            match &bus {
                Bus::Twi(dev) => {
                    let mut s = Mlx90640::new(Twi::open(dev.as_str(), mlx90640::DEFAULT_ADDR)?);
                    id = s.read_id()?;
                    status = s.read_status()?;
                    control = s.read_control()?;
                    ptat = s.read_ptat()?;
                }
                Bus::Gpio(sda, scl) => {
                    let mut s = Mlx90640::new(BitBang::new(*sda, *scl, mlx90640::DEFAULT_ADDR)?);
                    id = s.read_id()?;
                    status = s.read_status()?;
                    control = s.read_control()?;
                    ptat = s.read_ptat()?;
                }
            }
            println!("ID     0x240C = {id:#06x}");
            println!("status 0x8000 = {status:#06x}");
            println!("ctrl   0x800D = {control:#06x}");
            println!(
                "PTAT   0x0720 = {ptat}  (positive hundreds..thousands = healthy supply; \
                 see docs/mlx90640-temperature-math.md)"
            );
        }
        "ta" => {
            // Full compensation chain: EE decode -> aux/frame read -> To matrix.
            let (to, common): ([f32; mlx90640::PIXELS], ee::CommonParams);
            match &bus {
                Bus::Twi(dev) => {
                    let mut s = Mlx90640::new(Twi::open(dev, mlx90640::DEFAULT_ADDR)?);
                    let eew = s.read_eeprom()?;
                    let params = ee::Eeprom::from_words(&eew);
                    let aux = s.read_aux()?;
                    let frame = s.read_frame_raw()?;
                    let r = params.compute_to(&frame, &aux, 1.0, None);
                    to = r.0;
                    common = r.1;
                }
                Bus::Gpio(sda, scl) => {
                    let mut s = Mlx90640::new(BitBang::new(*sda, *scl, mlx90640::DEFAULT_ADDR)?);
                    let eew = s.read_eeprom()?;
                    let params = ee::Eeprom::from_words(&eew);
                    let aux = s.read_aux()?;
                    let frame = s.read_frame_raw()?;
                    let r = params.compute_to(&frame, &aux, 1.0, None);
                    to = r.0;
                    common = r.1;
                }
            }
            let min = to.iter().cloned().fold(f32::INFINITY, f32::min);
            let max = to.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
            let mean = to.iter().map(|v| f64::from(*v)).sum::<f64>() / to.len() as f64;
            println!(
                "Vdd={:.3}V  Ta={:.2}°C   To: n={} min={:.1} max={:.1} mean={:.1} °C",
                common.vdd,
                common.ta,
                to.len(),
                min,
                max,
                mean
            );
            for (y, row) in to.chunks(mlx90640::WIDTH).enumerate() {
                if y % 4 == 0 {
                    let line: Vec<String> =
                        row.iter().step_by(4).map(|v| format!("{v:5.0}")).collect();
                    println!("  {}", line.join(" "));
                }
            }
        }
        "frame" => {
            let frame = match &bus {
                Bus::Twi(dev) => {
                    Mlx90640::new(Twi::open(dev, mlx90640::DEFAULT_ADDR)?).read_frame_raw()?
                }
                Bus::Gpio(sda, scl) => {
                    Mlx90640::new(BitBang::new(*sda, *scl, mlx90640::DEFAULT_ADDR)?)
                        .read_frame_raw()?
                }
            };
            let min = frame.iter().min().copied().unwrap_or(0);
            let max = frame.iter().max().copied().unwrap_or(0);
            let n = frame.len();
            let mean = frame.iter().copied().map(i32::from).sum::<i32>() / n as i32;
            println!("frame: n={n} min={min} max={max} mean={mean}");
            for row in Mlx90640::<Twi>::downsample_8x6(&frame).iter() {
                let line: Vec<String> = row.iter().map(|v| format!("{v:5}")).collect();
                println!("  {}", line.join(" "));
            }
        }
        "serve" => run_serve(config_path)?,
        "scan" => {
            let mut gpio_candidates: Vec<(u32, u32)> = vec![(226, 227)];
            let args: Vec<String> = std::env::args().collect();
            if let Some(i) = args.iter().position(|a| a == "--gpio-candidates") {
                gpio_candidates =
                    scan::parse_gpio_candidates(args.get(i + 1).ok_or_else(|| {
                        error::Error::Usage("--gpio-candidates needs S,S:S,S".into())
                    })?)?;
            }
            println!(
                "scanning hardware TWI buses ({} devices)...",
                scan::twi_devices().len()
            );
            for d in &scan::twi_devices() {
                println!("  {d}: {}", bus_owner(d));
            }
            let mut hits = scan::scan_twi();
            println!("scanning GPIO bit-bang candidates {:?}...", gpio_candidates);
            for (sda, scl) in &gpio_candidates {
                let (found, pair_err) = scan::scan_gpio_pair(*sda, *scl);
                if let Some(e) = pair_err {
                    println!("  pair unusable — {e}");
                }
                hits.extend(found);
            }
            if hits.is_empty() {
                println!("no MLX90640 found — check wiring/VCC and see docs/porting.md");
            }
            for h in &hits {
                println!(
                    "\nfound MLX90640: bus={} addr={:#04x} id={:#06x}",
                    h.bus, h.addr, h.id
                );
                println!("config:\n{}", scan::config_snippet(h));
            }
        }
        other => return Err(error::Error::Usage(format!("unknown command: {other}"))),
    }
    Ok(())
}

/// IFrameCmd → next encoded frame is IDR (DeviceControl seam).
struct IframeHook {
    hub: std::sync::Arc<hub::ThermalHub>,
}

impl gb28181_rs::server::DeviceControlHandler for IframeHook {
    fn on_force_iframe(&self) {
        self.hub.request_idr();
    }
}

fn run_serve(config_path: Option<String>) -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let path = config_path.unwrap_or_else(|| "config.toml".into());
    let text = std::fs::read_to_string(&path)
        .map_err(|e| error::Error::Usage(format!("cannot read config {path}: {e}")))?;
    let cfg = settings::Settings::load_toml(&text)
        .map_err(|e| error::Error::Usage(format!("{path}: {e}")))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| error::Error::Usage(format!("tokio runtime: {e}")))?;

    runtime.block_on(async move {
        let hub = hub::ThermalHub::start_with_addr(&cfg.stream, &cfg.sensor.bus, cfg.sensor.addr);
        eprintln!(
            "thermal hub: {}x{} @ {:.1} fps, sensor bus {}",
            cfg.stream.width,
            cfg.stream.height,
            cfg.stream.fps,
            match &cfg.sensor.bus {
                settings::BusSpec::Twi(dev) => dev.clone(),
                settings::BusSpec::Gpio { sda, scl } => format!("gpio:{sda},{scl}"),
                settings::BusSpec::Auto => "auto".into(),
            }
        );

        let mut alarm_notifier: Option<httpsrv::AlarmNotifier> = None;

        let mut gb_handle = None;
        if cfg.gb28181.enabled {
            let au_source: std::sync::Arc<dyn gb28181_rs::frame::FrameSource> =
                std::sync::Arc::clone(&hub) as _;
            let server = gb28181_rs::server::Gb28181Server::new(cfg.gb28181.clone(), au_source)
                .with_control_handler(Some(std::sync::Arc::new(IframeHook {
                    hub: std::sync::Arc::clone(&hub),
                })));
            // Notifier must be captured before spawn() consumes the server.
            alarm_notifier = Some(server.notifier());
            match server.spawn().await {
                Ok(handle) => {
                    eprintln!("gb28181: registered (device {})", cfg.gb28181.device_id);
                    gb_handle = Some(handle);
                }
                Err(e) => eprintln!("gb28181: spawn failed: {e}"),
            }
        }

        let http_task = if cfg.http.enabled {
            let ctx = std::sync::Arc::new(httpsrv::WebCtx::new(
                std::sync::Arc::clone(&hub),
                alarm_notifier.clone(),
                cfg.http.password.clone(),
                cfg.http.api_token.clone(),
                std::path::PathBuf::from(&cfg.http.static_dir),
                cfg.gb28181.effective_device_name(),
            ));
            Some(tokio::spawn(async move {
                if let Err(e) = httpsrv::serve(ctx, cfg.http.port).await {
                    eprintln!("http server error: {e}");
                }
            }))
        } else {
            None
        };

        // Run until SIGINT/SIGTERM, then deregister if we ever registered.
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
        eprintln!("serve: shutting down");
        if let Some(handle) = gb_handle.as_mut() {
            let _ = handle.shutdown_with_deregister().await;
        }
        hub.stop();
        if let Some(task) = http_task {
            task.abort();
        }
    });
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
