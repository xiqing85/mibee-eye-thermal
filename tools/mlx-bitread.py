#!/usr/bin/env python3
"""Bit-bang MLX90640 register reader (16-bit reg addr). Pins must be GPIO-free."""
import sys, time

GB = "/sys/class/gpio"
D = 0.0004

class Pin:
    def __init__(s, n, name):
        s.n, s.name = n, name; s.p = f"{GB}/gpio{n}"
        open(f"{GB}/export", "w").write(str(n)); time.sleep(0.05)
        s.rz()
    def w(s, f, v): open(f"{s.p}/{f}", "w").write(str(v))
    def lo(s): s.w("direction", "out"); s.w("value", 0)
    def rz(s): s.w("direction", "in")
    def rd(s): return int(open(f"{s.p}/value").read())
    def un(s): open(f"{GB}/unexport", "w").write(str(s.n))

sda, scl = Pin(226, "PH2"), Pin(227, "PH3")

def start():
    sda.rz(); scl.rz(); time.sleep(2 * D)
    sda.lo(); time.sleep(D); scl.lo(); time.sleep(D)

def rstart():
    sda.rz(); time.sleep(D)
    scl.rz(); time.sleep(3 * D)
    sda.lo(); time.sleep(D); scl.lo(); time.sleep(D)

def stop():
    sda.lo(); time.sleep(D)
    scl.rz(); time.sleep(3 * D)
    sda.rz(); time.sleep(2 * D)

def wr_bit(b):
    if b: sda.rz()
    else: sda.lo()
    time.sleep(D)
    scl.rz(); time.sleep(3 * D)
    scl.lo(); time.sleep(D)

def rd_bit():
    sda.rz(); time.sleep(D)
    scl.rz(); time.sleep(3 * D)
    v = sda.rd()
    scl.lo(); time.sleep(D)
    return v

def send_byte(v):
    for i in range(7, -1, -1):
        wr_bit((v >> i) & 1)
    return rd_bit() == 0

def mack():
    sda.lo(); time.sleep(D)
    scl.rz(); time.sleep(3 * D)
    scl.lo(); time.sleep(D); sda.rz()

def mnak():
    sda.rz(); time.sleep(D)
    scl.rz(); time.sleep(3 * D)
    scl.lo(); time.sleep(D)

def read_reg16(addr, reg, nbytes=2):
    start()
    if not send_byte(addr << 1):        stop(); return None
    if not send_byte((reg >> 8) & 0xFF): stop(); return None
    if not send_byte(reg & 0xFF):       stop(); return None
    rstart()
    if not send_byte((addr << 1) | 1):  stop(); return None
    out = bytearray()
    for k in range(nbytes):
        b = 0
        for _ in range(8): b = (b << 1) | rd_bit()
        if k < nbytes - 1: mack()
        else: mnak()
        out.append(b)
    stop()
    return bytes(out)

A = 0x33
for name, reg in [("ID   0x240C", 0x240C), ("EEhdr 0x2400", 0x2400), ("ctrl 0x0702", 0x0702),
                  ("stat 0x0700", 0x0700), ("PTAT 0x0708", 0x0708)]:
    r = read_reg16(A, reg)
    if r and len(r) == 2:
        print(f"{name}: 0x{(r[0]<<8)|r[1]:04X}")
    else:
        print(f"{name}: FAIL/NAK")
sda.un(); scl.un()
