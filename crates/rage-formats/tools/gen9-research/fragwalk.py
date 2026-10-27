import sys, struct, zlib

def size_from_flags(f):
    s = (((f>>27)&1)<<0) + (((f>>26)&1)<<1) + (((f>>25)&1)<<2) + (((f>>24)&1)<<3) + (((f>>17)&0x7F)<<4) + (((f>>11)&0x3F)<<5) + (((f>>7)&0xF)<<6) + (((f>>5)&3)<<7) + (((f>>4)&1)<<8)
    return (0x200 << (f & 0xF)) * s

class R:
    def __init__(self, path):
        d = open(path, 'rb').read()
        magic, ver, sf, gf = struct.unpack_from('<4I', d)
        self.ver = ver; self.sf = sf; self.gf = gf
        body = zlib.decompressobj(-15).decompress(d[16:])
        ss, gs = size_from_flags(sf), size_from_flags(gf)
        self.sys = body[:ss]; self.gfx = body[ss:ss+gs]
        self.ss, self.gs = ss, gs
    def sl(self, va, n):
        if va & 0xF0000000 == 0x50000000: o = va - 0x50000000; return self.sys[o:o+n]
        if va & 0xF0000000 == 0x60000000: o = va - 0x60000000; return self.gfx[o:o+n]
        raise Exception('bad ptr %x' % va)
    def u64(self, va): return struct.unpack('<Q', self.sl(va, 8))[0]
    def u32(self, va): return struct.unpack('<I', self.sl(va, 4))[0]
    def u16(self, va): return struct.unpack('<H', self.sl(va, 2))[0]
    def u8(self, va): return self.sl(va, 1)[0]
    def f32(self, va): return struct.unpack('<f', self.sl(va, 4))[0]
    def s(self, va):
        if not va: return None
        b = self.sl(va, 256); return b[:b.index(0)].decode('latin1')

def mask(b):
    b = bytearray(b)
    for i in range(0, len(b) - 7, 8):
        v = struct.unpack_from('<Q', b, i)[0]
        if 0x50000000 <= v < 0x70000000: b[i:i+8] = b'PPPPPPPP'
    return bytes(b)

def walk(path):
    r = R(path)
    out = {}  # name -> (va, length, masked bytes)
    info = {'ver': r.ver, 'sysflags': hex(r.sf), 'gfxflags': hex(r.gf), 'sys': r.ss, 'gfx': r.gs}
    def blk(name, va, n):
        if va: out[name] = (va, n, mask(r.sl(va, n)))
    ft = 0x50000000
    blk('FragType', ft, 304)
    info['name'] = r.s(r.u64(ft + 0x58))
    info['radius'] = r.f32(ft + 0x2C)
    d = r.u64(ft + 0x30)
    blk('FragDrawable', d, 336)
    info['drawable_array_count'] = r.u32(ft + 0x48)
    info['cloths'] = r.u16(ft + 0x68)
    info['lights'] = r.u16(ft + 0x118)
    bt = r.u64(ft + 0xA8)
    if bt:
        n = r.u8(bt + 0x10); blk('BoneTransforms', bt, 32 + 48 * n); info['bone_transforms'] = n
    gwc = r.u8(ft + 0xD9); gwp = r.u64(ft + 0xE0)
    info['glass_windows'] = gwc
    if gwp:
        ptrs = [r.u64(gwp + 8 * i) for i in range(gwc)]
        info['glass_window_stride'] = (ptrs[1] - ptrs[0]) if len(ptrs) > 1 else None
        for i, p in enumerate(ptrs):
            # legacy 112; gen9 416 -> compare common head (0x40) and tail
            blk('GlassWindow[%d].head' % i, p, 0x40)
    vgw = r.u64(ft + 0x120)
    if vgw:
        total = r.u32(vgw + 8); blk('VehicleGlassWindows', vgw, total); info['vgw'] = (r.sl(vgw, 4), r.u16(vgw + 6), total)
    info['drawable_cloth'] = hex(r.u64(ft + 0xF8))
    # drawable
    if d:
        info['drawable_name'] = r.s(r.u64(d + 0x130))
        info['frag_matrices'] = (r.u16(d + 0x100), r.u16(d + 0x102), r.u16(d + 0x110))
        info['drawable.shadergroup'] = hex(r.u64(d + 0x10)); info['drawable.skeleton'] = hex(r.u64(d + 0x18))
        sk = r.u64(d + 0x18)
        if sk:
            blk('Skeleton', sk, 112)
            info['bones'] = r.u16(sk + 0x5E) if False else None
        db = r.u64(d + 0xF0)
        if db: info['drawable.bound_type'] = r.u8(db + 0x10); blk('Drawable.Bound.head', db, 0x80)
    plg = r.u64(ft + 0xF0)
    if plg:
        blk('PhysLODGroup', plg, 48)
        for li in range(3):
            lod = r.u64(plg + 0x10 + 8 * li)
            if not lod: continue
            p = 'LOD%d' % (li + 1)
            blk(p, lod, 304)
            gc = r.u8(lod + 0x11A); cc = r.u8(lod + 0x11D)
            info[p] = dict(groups=gc, children=cc, ud1=r.u8(lod + 0x118), ud2=r.u8(lod + 0x119))
            ab = r.u64(lod + 0x20)
            if ab:
                blk(p + '.ArtBody', ab, 176)
                jc = r.u8(ab + 0x89); jp = r.u64(ab + 0x78)
                info[p]['joints'] = jc
                for j in range(jc):
                    jv = r.u64(jp + 8 * j); t = r.u8(jv + 0x15)
                    blk(p + '.Joint[%d]' % j, jv, 176 if t == 0 else 240)
            gp = r.u64(lod + 0xC8)
            for g in range(gc):
                blk(p + '.Group[%d]' % g, r.u64(gp + 8 * g), 176)
            gn = r.u64(lod + 0xC0)
            if gn: blk(p + '.GroupNames', gn, gc * 8 + 8)
            cp = r.u64(lod + 0xD0)
            dsizes = set()
            for c in range(cc):
                ch = r.u64(cp + 8 * c)
                blk(p + '.Child[%d]' % c, ch, 256)
                for k, off in (('D1', 0xA0), ('D2', 0xA8)):
                    dv = r.u64(ch + off)
                    if dv:
                        blk(p + '.Child[%d].%s' % (c, k), dv, 336)
                ev = r.u64(ch + 0xB0)
                if ev: blk(p + '.Child[%d].Evt' % c, ev, 48)
            for an, off in (('Arch1', 0xD8), ('Arch2', 0xE0)):
                a = r.u64(lod + off)
                if a:
                    blk(p + '.' + an, a, 224)
                    info[p][an] = r.s(r.u64(a + 0x18))
            b = r.u64(lod + 0xE8)
            if b:
                info[p]['bound_type'] = r.u8(b + 0x10)
                blk(p + '.Bound.head', b, 0x80)
                # composite: children at 0x70 ptr, count u16 at 0x98?
            tr = r.u64(lod + 0x100)
            if tr:
                n = r.u32(tr + 0x10); blk(p + '.Transforms', tr, 32 + 64 * n)
            for nm, off, n in (('InertiaT', 0xF0, cc * 16), ('UnkVecs', 0xF8, cc * 16), ('UnkFloats', 0x28, cc * 4)):
                v = r.u64(lod + off)
                if v: blk(p + '.' + nm, v, n)
    return r, info, out

if __name__ == '__main__':
    a = walk(sys.argv[1])
    print(a[1])
    if len(sys.argv) > 2:
        b = walk(sys.argv[2])
        print(b[1])
        ka, kb = a[2], b[2]
        same = diff = 0
        for k in ka:
            if k not in kb: print('only in A', k); continue
            if ka[k][2] == kb[k][2]: same += 1
            else:
                diff += 1
                x, y = ka[k][2], kb[k][2]
                offs = [i for i in range(min(len(x), len(y))) if x[i] != y[i]]
                print('DIFF %-28s len %d/%d first diff offs %s' % (k, len(x), len(y), [hex(o) for o in offs[:12]]))
        for k in kb:
            if k not in ka: print('only in B', k)
        print('identical blocks:', same, 'different:', diff)
