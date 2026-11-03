# mibee-eye-thermal

[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.83%2B-orange.svg)](https://www.rust-lang.org/)

**MiBee Eye 热成像摄像机** —— MLX90640 远红外阵列（32×24）→ GB28181 / RTSP / Web，
面向无头 SBC（首发目标：Orange Pi Zero 3）。

MiBee Eye 摄像机家族成员：
[mibee-eye-rs](https://github.com/xiqing85/mibee-eye-rs) ·
[mibee-eye-go](https://github.com/xiqing85/mibee-eye-go) ·
[mibee-eye-hub](https://github.com/xiqing85/mibee-eye-hub) ·
[mibee-eye-webui](https://github.com/xiqing85/mibee-eye-webui)（共享前端 + API 规范）

> 状态：**pre-alpha / 硬件点灯期**。MLX90640 总线（TWI2 @ PH2/PH3，物理 8/10 脚）已在
> 真机上验证打通；驱动、热成像管线与 GB28181 设备栈建设中。Orange Pi Zero 3 的
> 关键坑（排针 I2C 与千兆网卡引脚冲突，为何绝不能用 TWI0/PI 引脚组）见
> `docs/hardware-notes.md`。

真源：内网 Gitea（`home-lab/mibee-eye-thermal`，PR 流程）。
公开镜像：[xiqing85/mibee-eye-thermal](https://github.com/xiqing85/mibee-eye-thermal)。

## 许可

Apache-2.0 —— 见 [LICENSE](LICENSE) 与 [NOTICE](NOTICE)。
