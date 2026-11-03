//! Userspace bit-banged I2C over sysfs GPIO — the bring-up path validated on
//! the Orange Pi Zero 3 (see `docs/hardware-notes.md`).
//!
//! The MLX90640 module there has no usable pull-ups, so the bus only closes
//! its timing at ~1 kHz — far below what hardware TWI will do, but enough to
//! read full frames (768 words in ~40 s) while the hardware is being fixed.
//! Timing constants mirror the field-proven python probe (`tools/mlx-frame.py`).

use crate::{Error, Result};
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
            File::create(base.join("export"))?
                .write_all(num.to_string().as_bytes())?;
            std::thread::sleep(Duration::from_millis(50));
        }
        let root = base.join(format!("gpio{num}"));
        let g = Self { num, root };
        g.rz(); // idle: released (input), pulled high
        Ok(g)
    }

    fn w(&self, name: &str, val: &str) -> Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .open(self.root.join(name))?;
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
    sda: Gpio,
    scl: Gpio,
}

fn d(n: u32) {
    sleep(D * n);
}

impl BitBang {
    /// `sda`/`scl` are global sysfs GPIO numbers (e.g. PH2=226, PH3=227 on
    /// the H616/H618 pinctrl at gpiochip0 base 0).
    pub fn new(sda: u32, scl: u32) -> Result<Self> {
        let sda = Gpio::new(sda)?;
        let scl = Gpio::new(scl)?;
        // Both lines must idle high — a stuck-low line means missing supply or
        // a short; report it explicitly instead of scanning into the void.
        d(2);
        if sda.rd()? == 0 || scl.rd()? == 0 {
            return Err(Error::Bus(
                "SDA or SCL stuck low: check module VCC/GND and pull-ups".into(),
            ));
        }
        Ok(Self { sda, scl })
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
        self.sda.rz()?;
        Ok(())
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
}

impl crate::i2c::MlxBus for BitBang {
    fn read_reg16(&mut self, reg: u16) -> Result<u16> {
        let mut b = [0u8; 2];
        self.read_block(reg, &mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    fn read_block(&mut self, reg: u16, buf: &mut [u8]) -> Result<()> {
        self.start()?;
        let ok = self.send_byte(crate::i2c::bitbang::ADDR << 1)? // placeholder, set below
            ;
        let _ = ok;
        unreachable!("replaced by read_block_at")
    }

    fn write_reg16(&mut self, _reg: u16, _val: u16) -> Result<()> {
        Err(Error::Usage("bitbang write not implemented yet".into()))
    }
}

impl BitBang {
    const ADDR7: u8 = 0x33;

    fn read_block_at(&mut self, addr: u8, reg: u16, buf: &mut [u8]) -> Result<()> {
        self.start()?;
        if !self.send_byte(addr << 1)? {
            self.stop()?;
            return Err(Error::Bus(format!("NAK addr {addr:#04x} (write)")));
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
            return Err(Error::Bus(format!("NAK addr {addr:#04x} (read)")));
        }
        for (i, slot) in buf.iter_mut().enumerate() {
            let mut b = 0u8;
            for _ in 0..8 {
                b = (b << 1) | self.rd_bit()?;
            }
            if i + 1 < buf.len() {
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
