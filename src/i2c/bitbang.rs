//! Userspace bit-banged I2C over sysfs GPIO — the bring-up path validated on
//! the Orange Pi Zero 3 (see `docs/hardware-notes.md`).
//!
//! The MLX90640 module there has no usable pull-ups, so the bus only closes
//! its timing at ~1 kHz — far below what hardware TWI will do, but enough to
//! read full frames (768 words in ~40 s) while the hardware is being fixed.
//! Timing constants mirror the field-proven python probe (`tools/mlx-frame.py`).

use crate::error::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

const D: Duration = Duration::from_micros(400); // ~1.6 kHz SCL

struct Gpio {
    num: u32,
    root: PathBuf,
}

impl Gpio {
    fn new(num: u32) -> Result<Self> {
        let base = PathBuf::from("/sys/class/gpio");
        if !base.join(format!("gpio{num}")).exists() {
            File::create(base.join("export"))?.write_all(num.to_string().as_bytes())?;
            sleep(Duration::from_millis(50));
        }
        let root = base.join(format!("gpio{num}"));
        let g = Self { num, root };
        g.rz()?; // idle: released (input), pulled high
        Ok(g)
    }

    fn w(&self, name: &str, val: &str) -> Result<()> {
        let mut f = OpenOptions::new().write(true).open(self.root.join(name))?;
        f.write_all(val.as_bytes())?;
        Ok(())
    }

    /// Drive the line low (open-drain "0").
    fn lo(&self) -> Result<()> {
        self.w("direction", "out")?;
        self.w("value", "0")
    }

    /// Release the line (input; pull-up brings it high).
    fn rz(&self) -> Result<()> {
        self.w("direction", "in")
    }

    fn rd(&self) -> Result<u8> {
        Ok(std::fs::read_to_string(self.root.join("value"))?
            .trim()
            .parse()
            .unwrap_or(0))
    }
}

impl Drop for Gpio {
    fn drop(&mut self) {
        if let Ok(mut f) = File::create("/sys/class/gpio/unexport") {
            let _ = f.write_all(self.num.to_string().as_bytes());
        }
    }
}

pub struct BitBang {
    addr: u8,
    sda: Gpio,
    scl: Gpio,
}

fn d(n: u32) {
    sleep(D * n);
}

/// Classic I2C bus recovery: toggle SCL with SDA released until the slave
/// lets SDA rise (≤32 clocks covers any reasonable outstanding transfer),
/// then generate a STOP so the slave resynchronizes to an idle bus.
fn recover_bus(sda: &Gpio, scl: &Gpio) -> Result<()> {
    for _ in 0..32 {
        scl.lo()?;
        d(1);
        scl.rz()?;
        d(2); // generous: weak pull-up rise is 240-510 µs
        if sda.rd()? != 0 {
            // SDA released — finish with a STOP condition.
            scl.lo()?;
            d(1);
            sda.lo()?;
            d(1);
            scl.rz()?;
            d(2);
            sda.rz()?;
            d(2);
            return Ok(());
        }
    }
    // Still low after 32 clocks — genuinely stuck hardware.
    Ok(())
}

impl BitBang {
    /// `sda`/`scl` are global sysfs GPIO numbers (PH2=226 / PH3=227 on the
    /// H616/H618 pinctrl at gpiochip0, base 0). `addr` is the 7-bit address
    /// (MLX90640 factory default 0x33).
    pub fn new(sda: u32, scl: u32, addr: u8) -> Result<Self> {
        let sda = Gpio::new(sda)?;
        let scl = Gpio::new(scl)?;
        // Both lines must idle high — a stuck-low SDA usually means the slave
        // is parked mid-transaction (a previous master died mid-block-read;
        // MLX90640 never times out and holds SDA low awaiting clocks). Clock
        // it out before declaring the bus dead: SCL toggles with SDA
        // released until SDA rises, then a STOP condition resyncs the slave.
        d(2);
        if sda.rd()? == 0 {
            recover_bus(&sda, &scl)?;
        }
        if scl.rd()? == 0 {
            return Err(Error::Bus(
                "SCL stuck low: check module VCC/GND and pull-ups".into(),
            ));
        }
        d(2);
        if sda.rd()? == 0 {
            return Err(Error::Bus(
                "SDA stuck low after bus recovery: check module VCC/GND and pull-ups".into(),
            ));
        }
        Ok(Self { addr, sda, scl })
    }

    fn start(&mut self) -> Result<()> {
        self.sda.rz()?;
        self.scl.rz()?;
        d(2);
        self.sda.lo()?;
        d(1);
        self.scl.lo()?;
        d(1);
        Ok(())
    }

    fn rstart(&mut self) -> Result<()> {
        self.sda.rz()?;
        d(1);
        self.scl.rz()?;
        d(3);
        self.sda.lo()?;
        d(1);
        self.scl.lo()?;
        d(1);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.sda.lo()?;
        d(1);
        self.scl.rz()?;
        d(3);
        self.sda.rz()?;
        d(2);
        Ok(())
    }

    fn wr_bit(&mut self, b: bool) -> Result<()> {
        if b {
            self.sda.rz()?;
        } else {
            self.sda.lo()?;
        }
        d(1);
        self.scl.rz()?;
        d(3);
        self.scl.lo()?;
        d(1);
        Ok(())
    }

    fn rd_bit(&mut self) -> Result<u8> {
        self.sda.rz()?;
        d(1);
        self.scl.rz()?;
        d(3);
        let v = self.sda.rd()?;
        self.scl.lo()?;
        d(1);
        Ok(v)
    }

    fn send_byte(&mut self, v: u8) -> Result<bool> {
        for i in (0..8).rev() {
            self.wr_bit(v >> i & 1 == 1)?;
        }
        Ok(self.rd_bit()? == 0) // slave ACK
    }

    fn mack(&mut self) -> Result<()> {
        self.sda.lo()?;
        d(1);
        self.scl.rz()?;
        d(3);
        self.scl.lo()?;
        d(1);
        self.sda.rz()
    }

    fn mnak(&mut self) -> Result<()> {
        self.sda.rz()?;
        d(1);
        self.scl.rz()?;
        d(3);
        self.scl.lo()?;
        d(1);
        Ok(())
    }

    fn read_block_at(&mut self, reg: u16, buf: &mut [u8]) -> Result<()> {
        let addr = self.addr;
        self.start()?;
        if !self.send_byte(addr << 1)? {
            self.stop()?;
            return Err(Error::Bus(format!("NAK addr {addr:#04x} (write phase)")));
        }
        if !self.send_byte((reg >> 8) as u8)? {
            self.stop()?;
            return Err(Error::Bus("NAK reg-hi".into()));
        }
        if !self.send_byte(reg as u8)? {
            self.stop()?;
            return Err(Error::Bus("NAK reg-lo".into()));
        }
        self.rstart()?;
        if !self.send_byte((addr << 1) | 1)? {
            self.stop()?;
            return Err(Error::Bus(format!("NAK addr {addr:#04x} (read phase)")));
        }
        let len = buf.len();
        for (i, slot) in buf.iter_mut().enumerate() {
            let mut b = 0u8;
            for _ in 0..8 {
                b = (b << 1) | self.rd_bit()?;
            }
            if i + 1 < len {
                self.mack()?;
            } else {
                self.mnak()?;
            }
            *slot = b;
        }
        self.stop()?;
        Ok(())
    }
}

impl crate::i2c::MlxBus for BitBang {
    fn read_reg16(&mut self, reg: u16) -> Result<u16> {
        let mut b = [0u8; 2];
        self.read_block_at(reg, &mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    fn read_block(&mut self, reg: u16, buf: &mut [u8]) -> Result<()> {
        self.read_block_at(reg, buf)
    }

    fn write_reg16(&mut self, _reg: u16, _val: u16) -> Result<()> {
        // Only needed later (refresh-rate / chess-mode configuration); the
        // bring-up path is read-only.
        Err(Error::Usage("bitbang write not implemented yet".into()))
    }
}
