#!/usr/bin/env python3
"""Dump one full MLX90640 RAM frame (0x0400..0x06FF) via bit-bang. ~17s."""
import sys, time
exec(open("/root/mlx-bitread.py").read().split('A = 0x33')[0])  # reuse primitives

def dump_frame(tag):
    data = bytearray()
    # read in 4 chunks to bound transaction length
    for base in (0x0400, 0x0480, 0x0500, 0x0580, 0x0600, 0x0680):
        chunk = read_reg16(0x33, base, 256)
        if chunk is None:
            print(f"{tag}: chunk {hex(base)} NAK"); return None
        data += chunk
    words = [(data[i] << 8) | data[i+1] for i in range(0, len(data), 2)]
    s16 = [w - 65536 if w > 32767 else w for w in words]
    print(f"{tag}: n={len(s16)} min={min(s16)} max={max(s16)} mean={sum(s16)/len(s16):.0f}")
    # 8x6 downsample of the 32x24 grid (row-major)
    for gy in range(6):
        row = []
        for gx in range(8):
            acc = 0; cnt = 0
            for y in range(gy*4, gy*4+4):
                for x in range(gx*4, gx*4+4):
                    acc += s16[y*32+x]; cnt += 1
            row.append(int(acc/cnt))
        print("  " + " ".join(f"{v:5d}" for v in row))
    return s16

t0 = time.time()
f1 = dump_frame("frame1")
time.sleep(2)
f2 = dump_frame("frame2")
if f1 and f2:
    diff = sum(abs(a-b) for a, b in zip(f1, f2)) / len(f1)
    print(f"mean|frame2-frame1| = {diff:.1f}  (>{5} = 数据在流动)")
print(f"elapsed {time.time()-t0:.0f}s")
sda.un(); scl.un()
