"""Misc Gen9 vs legacy checks on the same vanilla yft: FragType scalar fields,
vehicle glass windows block, frag drawable bound, model bindings, drawable bounds."""
import sys, struct
from g9probe import Probe
g9 = Probe(sys.argv[1]); g9.walk_yft(); lg = Probe(sys.argv[2]); lg.walk_yft()
r9, rl = g9.r, lg.r; F = 0x50000000
def scal(r):  # FragType non-pointer fields 0xB0..0xDC
    return r.b(F + 0xB0, 0x2C)
print("FragType 0xB0..0xDB equal:", scal(r9) == scal(rl), struct.unpack("<6i3f", scal(r9)[:36]), struct.unpack("<6i3f", scal(rl)[:36]))
for p, r in ((g9, r9), (lg, rl)):
    v = p.frag["vglass_ptr"]
    if v:
        tl = r.u32(v + 8); cnt = r.u16(v + 6)
        p.vgw = r.b(v, tl); print("VGW magic", r.b(v, 4), "count", cnt, "totallen", tl)
    else: p.vgw = None
if g9.vgw and lg.vgw:
    print("VGW blocks byte-identical:", g9.vgw == lg.vgw, "len", len(g9.vgw), len(lg.vgw))
    diffs = [i for i in range(len(g9.vgw)) if g9.vgw[i] != lg.vgw[i]]
    print("VGW differing byte count", len(diffs), "first offsets", diffs[:40])
# frag drawable bound
for p, r in ((g9, r9), (lg, rl)):
    d = r.u64(F + 0x30); b = r.u64(d + 0xF0)
    print("frag drawable bound ptr 0x%X" % b)
    p.bnd = r.b(b + 0x10, 0x60) if b else None
print("bound header 0x10..0x70 equal:", g9.bnd == lg.bnd)
# model bindings/masks and drawable bboxes
for (va9, d9), (val, dl) in zip(g9.drawables.items(), lg.drawables.items()):
    a = r9.b(va9 + 0x20, 0x30) + r9.b(va9 + 0x70, 0x20); b = rl.b(val + 0x20, 0x30) + rl.b(val + 0x70, 0x20)
    if a != b: print("drawable bounds/lod/mask differ", d9["labels"])
    for lod in d9["lods"]:
        for m9, ml in zip(d9["lods"][lod], dl["lods"][lod]):
            if (m9["skelbind"], m9["rendermask"]) != (ml["skelbind"], ml["rendermask"]): print("model binding differs", m9["label"])
print("drawable/model field checks done")
