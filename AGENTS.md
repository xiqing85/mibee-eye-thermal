# AGENTS.md — mibee-eye-thermal

MiBee Eye 家族的热成像成员：MLX90640（32×24 远红外阵列）→ GB28181/RTSP/Web，
首发部署目标 Orange Pi Zero 3（H618、`orangepizero3`、192.168.62.40，双链路
62.40 有线 / 62.118 WiFi MickeyMiRoute）。

**先读工作区根 `../AGENTS.md`**（家族总纲、同步发布规范、管辖边界）。本文件只记
本仓特有事实。

## 仓库拓扑

- **真源**：内网 Gitea `home-lab/mibee-eye-thermal`（remote `origin`，main 直推允许，家族惯例）
- **公开镜像**：`xiqing85/mibee-eye-thermal`（remote `github`，走 `github-xiqing85`
  SSH 别名/id_rsa；main 已保护：PR-only + `check` CI + 管理员生效 + 线性历史）
- 镜像流程：Gitea main 推完 → `git push github main`（新仓历史一致，无需 rs/go 的
  脱敏快照舞步）→ 若直推被保护拦截就开 PR 走 CI 再 squash 合并
- GitHub 侧 issue tracker 为主对外门面；**NVR 对接等内部协作在内网 Gitea issue**
  （用户定规 2026-11-03）

## 硬件事实（HARD，都是踩过的坑）

见 `docs/hardware-notes.md` 全文。速记：

1. **Zero 3 排针 3/5 脚（TWI0/PI5/PI6）与千兆网卡 RGMII 引脚物理复用**——启用任何
   `*-pi` I2C overlay = `end0` 整个消失（dmesgl: "pin PI5 already requested …
   cannot claim for 5020000.ethernet"）。**只能用 PH2/PH3（物理 8/10 脚，TWI2，
   `overlays=i2c2-ph`）**
2. 模块无有效上拉：实测上升沿 240–510µs，硬件 TWI 100k/25k 全 NAK，只有 ~1.6kHz
   sysfs 位模拟能通（0x33，768 字整帧 ~40s）。**外接 2×4.7kΩ 上拉后**硬件 TWI 解锁
   100–400kHz（慢速 overlay 源码在 `deploy/overlays/`）
3. **数据完整性未决**：帧数据空间结构真实稳定，但全 0xFFxx + PTAT 负值——疑似模块
   欠压（带 LDO 的模块必须接 5V/2 脚）。绝对温度在解决前不可信
4. 板子实验前确保 WiFi 备份链路活着（netplan 20-wifi-backup.yaml 已常驻）；动
   armbianEnv.txt 前 `sync` 并当场 cat 验证（2026-11-03 的 sed 未生效空重启事故）

## 代码结构

- `src/i2c/`：`MlxBus` trait + 两实现——`twi`（/dev/i2c-N，I2C_RDWR 组合传输，零依赖
  手写 ioctl）与 `bitbang`（sysfs GPIO 位模拟，字段验证过的时序参数，勿乱调 D 常量）
- `src/mlx90640.rs`：寄存器图 + 身份/状态/整帧读取；**EE 补偿算法（逐像素温度）是
  后续里程碑**，按 datasheet 清白实现，勿引入 Melexis SDK 代码（NOTICE 声明约束）
- `tools/mlx-*.py`：板上验证过的探针（位模拟扫地址双极性/寄存器读/整帧 dump），
  排障时先 unbind TWI2 再用

## 开发日志

工作区定规（2026-11-03）：本仓一切实质进展/难点/决策当天写入 `../mibee-dev-journal/teams/eye/<YYYY-MM>.md` 并推送（规范见该仓 00-规范；凭据永不入仓）。

## 对接 MiBeeNVR（.30）路线

内网 Gitea issue 跟踪（总跟踪 issue #1）：GB28181 设备注册（gb28181-rs pin 最新
main）、Catalog 单通道（热成像通道）、INVITE 起流（软编码 H.264，伪彩色 32×24 →
放大渲染）、温度越限 → Alarm NOTIFY（家族 AlarmBridge 模式）。SPEC v1 API + webui
卡片在服务里程碑接入。
