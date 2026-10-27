"""Compare the physics LOD1 composite bound (and each child bound) between Gen9 and legacy:
type, header bytes 0x10..0x70, and for geometry/BVH children vertex/polygon counts and raw vertex bytes."""
import sys
from g9probe import Probe
def walk(p):
    r = p.r; grp = r.u64(0x50000000 + 0xF0); lod = r.u64(grp + 0x10); b = r.u64(lod + 0xE8)
    out = [("composite", r.u8(b + 0x10), r.b(b + 0x10, 0x60))]
    cp = r.u64(b + 0x70); n = r.u16(b + 0xA0)
    for i in range(n):
        c = r.u64(cp + 8 * i)
        if not c: out.append(("child%d" % i, None, None, None)); continue
        t = r.u8(c + 0x10); hdr = r.b(c + 0x10, 0x60)
        extra = None
        if t in (4, 8):  # geometry / BVH
            vp = r.u64(c + 0x78); pp = r.u64(c + 0x88); vc = r.u32(c + 0xF0 + 0x18) if False else None
            extra = (r.b(c + 0x70, 0xC0),)
        out.append(("child%d" % i, t, hdr, extra))
    return out
a = walk(Probe(sys.argv[1])); b = walk(Probe(sys.argv[2]))
print("children: gen9=%d legacy=%d" % (len(a) - 1, len(b) - 1))
for x, y in zip(a, b):
    print("%-10s type %s/%s header0x10-0x70 equal=%s" % (x[0], x[1], y[1], x[2] == y[2]))
