# Device-tree overlays — Orange Pi Zero 3 (H616/H618)

Stock `i2c2-ph` enables TWI2 on PH2/PH3 (header pins 8/10) at the default
100 kHz. On weak-pull-up wiring the bus needs to go slower; these variants
patch `clock-frequency` into the controller node. Build on the board
(`apt install device-tree-compiler`):

    dtc -I dts -O dtb -o sun50i-h616-i2c2-ph-25k.dtbo i2c2-ph-25k.dts

Install into `/boot/dtb/allwinner/overlay/` and set in `/boot/armbianEnv.txt`:

    overlays=i2c2-ph-25k      (or i2c2-ph-10k, or stock i2c2-ph once
                               external pull-ups are fitted)

See `docs/hardware-notes.md` for why the `i2c0-pi` / `i2c1-pi` / `i2c2-pi`
overlays must never be used on this board (RGMII conflict kills end0).
