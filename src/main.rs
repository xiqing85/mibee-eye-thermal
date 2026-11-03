//! mibee-eye-thermal — MLX90640 thermal camera for the MiBee Eye family.
//!
//! Bring-up CLI (v0.0.x):
//!   mibee-eye-thermal id     --bus twi:/dev/i2c-2
//!   mibee-eye-thermal frame  --bus gpio:226,227
//! The service build (web/RTSP/GB28181) layers on top of the same driver.

mod error;
mod i2c;
mod mlx90640;

use error::Result;
use i2c::{bitbang::BitBang, twi::Twi};
use mlx90640::Mlx90640;

enum Bus {
    Twi(String),
    Gpio(u32, u32),
}

fn parse_args() -> Result<(String, Bus)> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args
        .get(1)
        .ok_or_else(|| {
            error::Error::Usage(
                "usage: mibee-eye-thermal <id|frame> [--bus <twi:/dev/i2c-N|gpio:SDA,SCL>]".into(),
            )
        })?
        .clone();
    let mut bus = Bus::Twi("/dev/i2c-2".into());
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
            other => return Err(error::Error::Usage(format!("unknown arg: {other}"))),
        }
        i += 1;
    }
    Ok((cmd, bus))
}

fn run() -> Result<()> {
    let (cmd, bus) = parse_args()?;
    match cmd.as_str() {
        "id" => {
            let (id, status, control, ptat): (u16, u16, u16, i16);
            match &bus {
                Bus::Twi(dev) => {
                    let mut s = Mlx90640::new(Twi::open(dev, mlx90640::DEFAULT_ADDR)?);
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
            println!("status 0x0700 = {status:#06x}");
            println!("ctrl   0x0702 = {control:#06x}");
            println!(
                "PTAT   0x0708 = {ptat}  (negative => suspect supply, see docs/hardware-notes.md)"
            );
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
        other => return Err(error::Error::Usage(format!("unknown command: {other}"))),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
