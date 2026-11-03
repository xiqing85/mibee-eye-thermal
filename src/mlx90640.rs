//! MLX90640 32x24 far-infrared array — register map, identity, raw frames.
//!
//! Frame-extraction maths (EE calibration coefficients -> per-pixel To) is a
//! later milestone; this layer provides the validated register-level access:
//! identity read, status polling, full 768-word RAM frame reads.

use crate::i2c::MlxBus;
use crate::Result;

pub const WIDTH: usize = 32;
pub const HEIGHT: usize = 24;
pub const PIXELS: usize = WIDTH * HEIGHT;

pub const DEFAULT_ADDR: u8 = 0x33;

// Register map (MLX90640 datasheet, RAM is word-addressed big-endian).
pub const RAM_IR_BASE: u16 = 0x0400;
pub const REG_STATUS: u16 = 0x0700;
pub const REG_CONTROL: u16 = 0x0702;
pub const REG_PTAT: u16 = 0x0708;
/// Calibration block — lands with the To-extraction milestone.
#[allow(dead_code)]
pub const EE_BASE: u16 = 0x2400;
pub const REG_ID: u16 = 0x240C;

pub struct Mlx90640<B: MlxBus> {
    bus: B,
}

impl<B: MlxBus> Mlx90640<B> {
    pub fn new(bus: B) -> Self {
        Self { bus }
    }

    /// Identity register. A NAK here means wiring/address trouble; a plausible
    /// non-0xFFFF value means the protocol layer is alive.
    pub fn read_id(&mut self) -> Result<u16> {
        self.bus.read_reg16(REG_ID)
    }

    pub fn read_status(&mut self) -> Result<u16> {
        self.bus.read_reg16(REG_STATUS)
    }

    pub fn read_control(&mut self) -> Result<u16> {
        self.bus.read_reg16(REG_CONTROL)
    }

    /// Die temperature proxy; must be a positive count on a healthy sensor —
    /// a negative PTAT is the classic undervoltage tell (see hardware notes).
    pub fn read_ptat(&mut self) -> Result<i16> {
        Ok(self.bus.read_reg16(REG_PTAT)? as i16)
    }

    /// Read one full IR frame (768 raw signed words, row-major 32x24).
    pub fn read_frame_raw(&mut self) -> Result<[i16; PIXELS]> {
        let mut words = [0u16; PIXELS];
        self.bus.read_words(RAM_IR_BASE, &mut words)?;
        Ok(words.map(|w| w as i16))
    }

    /// Coarse 8x6 view of a frame (block means) — the same projection used
    /// during bring-up, handy for quick eyeballing in logs.
    pub fn downsample_8x6(frame: &[i16; PIXELS]) -> [[i32; 8]; 6] {
        let mut out = [[0i32; 8]; 6];
        for (gy, row) in out.iter_mut().enumerate() {
            for (gx, cell) in row.iter_mut().enumerate() {
                let mut acc = 0i32;
                for y in gy * 4..gy * 4 + 4 {
                    for x in gx * 4..gx * 4 + 4 {
                        acc += i32::from(frame[y * WIDTH + x]);
                    }
                }
                *cell = acc / 16;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_block_means() {
        let mut f = [0i16; PIXELS];
        f[0] = 16; // top-left pixel only
        f[PIXELS - 1] = 32; // bottom-right pixel only
        let d = Mlx90640::<crate::i2c::twi::Twi>::downsample_8x6(&f);
        assert_eq!(d[0][0], 1);
        assert_eq!(d[5][7], 2);
        assert_eq!(d[2][3], 0);
    }
}
