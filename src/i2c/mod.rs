//! Transport-agnostic MLX90640 bus contract.
//!
//! MLX90640 uses 16-bit register addressing with SMBus-style word/block reads:
//! `START, addr+W, regH, regL, RESTART, addr+R, ...data..., STOP`.
//! Two implementations back this trait:
//! - [`twi`]: kernel hardware I2C (`/dev/i2c-N`) — preferred once the bus
//!   has proper pull-ups (see `docs/hardware-notes.md`).
//! - [`bitbang`]: userspace sysfs-GPIO bit-bang — the proven bring-up path on
//!   boards where the module lacks pull-ups and the bus must run at ~1 kHz.

pub mod bitbang;
pub mod twi;

pub trait MlxBus {
    /// Read a single 16-bit register (big-endian on the wire).
    fn read_reg16(&mut self, reg: u16) -> crate::Result<u16>;

    /// Read `buf.len()` bytes starting at `reg` (auto-incrementing block read).
    fn read_block(&mut self, reg: u16, buf: &mut [u8]) -> crate::Result<()>;

    /// Write a single 16-bit register.
    /// Reserved for the configuration milestone (refresh rate / chess mode);
    /// bring-up is read-only.
    #[allow(dead_code)]
    fn write_reg16(&mut self, reg: u16, val: u16) -> crate::Result<()>;

    /// Read a run of 16-bit words into `out` (block read, chunked).
    fn read_words(&mut self, base: u16, out: &mut [u16]) -> crate::Result<()> {
        // 256-byte chunks validated on real hardware (32x24 full-frame dumps).
        const CHUNK_WORDS: usize = 128;
        let mut byte = 0usize;
        while byte < out.len() * 2 {
            let reg = base + (byte / 2) as u16;
            let take = ((out.len() * 2 - byte) / 2).min(CHUNK_WORDS) * 2;
            let mut tmp = vec![0u8; take];
            self.read_block(reg, &mut tmp)?;
            for (i, w) in tmp.chunks_exact(2).enumerate() {
                out[byte / 2 + i] = u16::from_be_bytes([w[0], w[1]]);
            }
            byte += take;
        }
        Ok(())
    }
}
