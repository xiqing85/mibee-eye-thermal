//! Hardware I2C via `/dev/i2c-N` (combined I2C_RDWR transfers).
//!
//! Zero external deps: raw `ioctl(I2C_RDWR)` with the 64-bit UABI structs.
//! MLX90640 needs a combined write(2B reg addr) + repeated-start read —
//! exactly one `i2c_rdwr_ioctl_data` with two messages.

use crate::{Error, Result};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;

const I2C_RDWR: libc::c_ulong = 0x0707;
const I2C_M_RD: u16 = 0x0001;

#[repr(C)]
struct I2cMsg {
    addr: u16,
    flags: u16,
    len: u16,
    _pad: u16,
    buf: *mut u8,
}

#[repr(C)]
struct I2cRdwrIoctlData {
    msgs: *mut I2cMsg,
    nmsgs: libc::c_uint,
}

pub struct Twi {
    file: File,
    addr: u16,
}

impl Twi {
    pub fn open(path: &str, addr: u16) -> Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Ok(Self { file, addr })
    }

    /// One ioctl: [write regH,regL] + repeated-start + [read n bytes].
    fn wr_rd(&mut self, reg: u16, rd_len: usize, wr_extra: &[u8]) -> Result<Vec<u8>> {
        let mut wr = vec![(reg >> 8) as u8, reg as u8];
        wr.extend_from_slice(wr_extra);
        let mut out = vec![0u8; rd_len];
        let mut msgs = [
            I2cMsg {
                addr: self.addr,
                flags: 0,
                len: wr.len() as u16,
                _pad: 0,
                buf: wr.as_mut_ptr(),
            },
            I2cMsg {
                addr: self.addr,
                flags: I2C_M_RD,
                len: rd_len as u16,
                _pad: 0,
                buf: out.as_mut_ptr(),
            },
        ];
        let data = I2cRdwrIoctlData {
            msgs: msgs.as_mut_ptr(),
            nmsgs: 2,
        };
        // SAFETY: fd is a live i2c-dev handle; pointers point at the buffers above.
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), I2C_RDWR, &data) };
        if rc < 0 {
            return Err(Error::Bus(format!(
                "I2C_RDWR {:#06x} ({}B read): {}",
                reg,
                rd_len,
                std::io::Error::last_os_error()
            )));
        }
        Ok(out)
    }
}

impl crate::i2c::MlxBus for Twi {
    fn read_reg16(&mut self, reg: u16) -> Result<u16> {
        let b = self.wr_rd(reg, 2, &[])?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn read_block(&mut self, reg: u16, buf: &mut [u8]) -> Result<()> {
        buf.copy_from_slice(&self.wr_rd(reg, buf.len(), &[])?);
        Ok(())
    }

    fn write_reg16(&mut self, reg: u16, val: u16) -> Result<()> {
        let mut wr = vec![(reg >> 8) as u8, reg as u8];
        wr.extend_from_slice(&val.to_be_bytes());
        let mut msgs = [I2cMsg {
            addr: self.addr,
            flags: 0,
            len: wr.len() as u16,
            _pad: 0,
            buf: wr.as_mut_ptr(),
        }];
        let data = I2cRdwrIoctlData {
            msgs: msgs.as_mut_ptr(),
            nmsgs: 1,
        };
        let rc = unsafe { libc::ioctl(self.file.as_raw_fd(), I2C_RDWR, &data) };
        if rc < 0 {
            return Err(Error::Bus(format!(
                "I2C_RDWR write {:#06x}: {}",
                reg,
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}
