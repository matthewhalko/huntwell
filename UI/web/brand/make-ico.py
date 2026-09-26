"""Wrap PNGs into ../public/favicon.ico: ICO may hold PNG data directly
(every browser, and Windows since Vista). A 6-byte header, a 16-byte entry per
image, then the PNG bytes.

    sips -z 16 16 favicon-512.png --out fav-16.png   # and 32, 48
    python3 make-ico.py fav-16.png fav-32.png fav-48.png
"""
import struct
import sys

paths = sys.argv[1:]
images = []
for p in paths:
    data = open(p, "rb").read()
    width, height = struct.unpack(">II", data[16:24])  # the PNG IHDR
    images.append((width, height, data))

out = struct.pack("<HHH", 0, 1, len(images))
offset = 6 + 16 * len(images)
for w, h, data in images:
    out += struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
for _, _, data in images:
    out += data
open("../public/favicon.ico", "wb").write(out)
