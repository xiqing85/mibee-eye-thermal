# Changelog

All notable changes to this project will be documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.0.1] — 2026-11-03

### Added
- Project bootstrap: bilingual README, Apache-2.0 LICENSE + NOTICE, CI
  (`check`: fmt / clippy -D warnings / test / release build).
- `MlxBus` transport trait with two implementations:
  - `twi` — kernel hardware I2C via raw `ioctl(I2C_RDWR)` combined transfers;
  - `bitbang` — sysfs-GPIO bit-bang at ~1.6 kHz (the only electrically working
    path on the bring-up board until external pull-ups are fitted).
- `mlx90640` register layer: identity / status / PTAT / full 768-word raw
  frames, 8x6 downsample helper.
- Bring-up CLI: `id` and `frame` commands over either bus.
- `docs/hardware-notes.md`: Orange Pi Zero 3 pin-conflict findings (header
  TWI0/PI pins vs gigabit RGMII), measured rise times, slow-bus overlays.
- `tools/`: field-validated python probes (polarity address scan, register
  reads, frame dump).
- `deploy/overlays/`: i2c2-ph slow-speed (25k/10k) overlay sources.

### Known limitations
- Absolute temperatures untrusted until the module supply question is
  resolved (see hardware notes — PTAT negative, words read 0xFFxx).
- Bit-bang bus ~40 s/frame; hardware TWI blocked on external pull-ups.
