# mibee-eye-thermal

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.83%2B-orange.svg)](https://www.rust-lang.org/)

**MiBee Eye thermal imaging camera** — MLX90640 far-infrared array (32x24) to
GB28181 / RTSP / Web, sized for headless SBCs (first target: Orange Pi Zero 3).

Part of the MiBee Eye camera family:
[mibee-eye-rs](https://github.com/xiqing85/mibee-eye-rs) ·
[mibee-eye-go](https://github.com/xiqing85/mibee-eye-go) ·
[mibee-eye-hub](https://github.com/xiqing85/mibee-eye-hub) ·
[mibee-eye-webui](https://github.com/xiqing85/mibee-eye-webui) (shared frontend + API spec)

> Status: **pre-alpha / hardware bring-up**. The MLX90640 bus (TWI2 on PH2/PH3)
> is validated on real hardware; driver, thermal pipeline and GB28181 device
> stack are under construction. See `docs/hardware-notes.md` for the
> Orange Pi Zero 3 pin-conflict findings (why the header I2C must NOT use
> TWI0/PI pins on that board).

True source: intranet Gitea (`home-lab/mibee-eye-thermal`, PR flow).
Public mirror: [xiqing85/mibee-eye-thermal](https://github.com/xiqing85/mibee-eye-thermal).

## License

Apache-2.0 — see [LICENSE](LICENSE) and [NOTICE](NOTICE).
