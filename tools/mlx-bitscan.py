#!/usr/bin/env python3
"""Bit-bang I2C address scan on two sysfs GPIOs, both SDA/SCL polarities."""
import sys, time

GB = "/sys/class/gpio"
DELAY = 0.0006  # ~1-2 kHz SCL; I2C has no minimum clock

class Pin:
    def __init__(self, num, name):
        self.num, self.name = num, name
        self.p = f"{GB}/gpio{num}"
        open(f"{GB}/export", "w").write(str(num)); time.sleep(0.05)
    def w(self, f, v): open(f"{self.p}/{f}", "w").write(str(v))
    def drive0(self): self.w("direction", "out"); self.w("value", 0)
    def release(self): self.w("direction", "in")
    def read(self): return int(open(f"{self.p}/value").read())
    def unexport(self): open(f"{GB}/unexport", "w").write(str(self.num))

def scan(sda, scl, verbose=False):
    sda.release(); scl.release(); time.sleep(2 * DELAY)
    print(f"  idle levels: SDA({sda.name})={sda.read()} SCL({scl.name})={scl.read()}")
    if sda.read() == 0 or scl.read() == 0:
        print("  !! line stuck LOW — no pull-up or short to GND"); return []
    found = []
    for addr in range(0x08, 0x78):
        # START
        sda.drive0(); time.sleep(DELAY)
        scl.drive0(); time.sleep(DELAY)
        ack = None
        # send (addr<<1)|1 read-opcode, MSB first
        for bit in range(7, -1, -1):
            b = ((addr << 1) | 1) >> bit & 1
            if b: sda.release()
            else: sda.drive0()
            time.sleep(DELAY)
            scl.release(); time.sleep(2 * DELAY)
            scl.drive0(); time.sleep(DELAY)
        # ACK slot: release SDA, clock high, sample
        sda.release(); time.sleep(DELAY)
        scl.release(); time.sleep(2 * DELAY)
        ack = (sda.read() == 0)
        scl.drive0(); time.sleep(DELAY)
        # STOP
        sda.drive0(); time.sleep(DELAY)
        scl.release(); time.sleep(2 * DELAY)
        sda.release(); time.sleep(DELAY)
        if ack: found.append(addr)
    return found

PH2, PH3 = 226, 227
print(f"=== polarity A: SDA=PH2(pin8)  SCL=PH3(pin10)  [接法正确时应命中] ===")
a = Pin(PH2, "PH2"); b = Pin(PH3, "PH3")
fa = scan(a, b)
print(f"  ACK at: {[hex(x) for x in fa] if fa else '无'}")
a.unexport(); b.unexport()
print(f"=== polarity B: SDA=PH3(pin10) SCL=PH2(pin8)   [接反时应命中] ===")
a = Pin(PH3, "PH3"); b = Pin(PH2, "PH2")
fb = scan(a, b)
print(f"  ACK at: {[hex(x) for x in fb] if fb else '无'}")
a.unexport(); b.unexport()
print()
print("VERDICT:", "接法正确(SDA=PH2,SCL=PH3)" if fa and not fb else
                 ("接反了(SDA=PH3,SCL=PH2)" if fb and not fa else
                 ("两极性都有应答(异常)" if fa or fb else "两极性都无应答: 查VCC/GND或没接到8/10脚")))
