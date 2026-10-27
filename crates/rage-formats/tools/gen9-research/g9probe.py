"""Throwaway probe: walk a .yft/.ydr (legacy or Gen9) down to geometries and
report vertex declarations, index buffers, shaders, skeleton.

usage: python g9probe.py <file> [--json out.json]
Spec: the reference library Drawable.cs / Frag.cs (layouts), verified empirically here.
"""
import sys, struct, zlib, json, re, os, collections
import xml.etree.ElementTree as ET

CW = r"C:\dev\gta\v\tools\reference\source\the reference library\ShadersGen9Conversion.xml"
NAMES_DIR = r"C:\dev\gta\v\tools\rage-formats\src\names"

def joaat(s):
    h = 0
    for c in s.lower().encode():
        h = (h + c) & 0xFFFFFFFF
        h = (h + (h << 10)) & 0xFFFFFFFF
        h ^= h >> 6
    h = (h + (h << 3)) & 0xFFFFFFFF
    h ^= h >> 11
    h = (h + (h << 15)) & 0xFFFFFFFF
    return h

NAMES = {}
def addname(n):
    if n: NAMES.setdefault(joaat(n), n)

# ---- conversion XML ----
CONV = {}  # shader hash -> dict(name, params: {g9hash: (type,name,old)})
def load_conv():
    root = ET.parse(CW).getroot()
    for it in root.findall("Item"):
        name = it.findtext("Name")
        addname(name); addname(it.findtext("FileName"))
        ps = {}
        for p in it.find("Parameters").findall("Item"):
            pn = p.get("name"); old = p.get("old")
            if pn.lower().startswith("hash_"):
                h = int(pn[5:], 16)
            else:
                h = joaat(pn); addname(pn)
            addname(old)
            ps[h] = (p.get("type"), pn, old)
        CONV[joaat(name)] = dict(name=name, params=ps, bufsizes=it.findtext("BufferSizes"))
load_conv()
for f in ("shader_params.txt", "reference.txt"):
    p = os.path.join(NAMES_DIR, f)
    if os.path.exists(p):
        for line in open(p, encoding="utf-8", errors="ignore"):
            line = line.strip()
            if line and not line.startswith("#"): addname(line)
def nm(h):
    return NAMES.get(h, "0x%08X" % h)

# ---- RSC7 ----
def size_from_flags(f):
    s = (((f >> 27) & 1) << 0) + (((f >> 26) & 1) << 1) + (((f >> 25) & 1) << 2) + (((f >> 24) & 1) << 3) \
        + (((f >> 17) & 0x7F) << 4) + (((f >> 11) & 0x3F) << 5) + (((f >> 7) & 0xF) << 6) + (((f >> 5) & 3) << 7) + (((f >> 4) & 1) << 8)
    return (0x200 << (f & 0xF)) * s

class Res:
    def __init__(self, path):
        d = open(path, "rb").read()
        magic, ver, sf, gf = struct.unpack_from("<IIII", d, 0)
        assert magic == 0x37435352, hex(magic)
        self.version = (((sf >> 28) & 0xF) << 4) | ((gf >> 28) & 0xF)
        self.hdr_ver = ver
        self.ss, self.gs = size_from_flags(sf), size_from_flags(gf)
        body = zlib.decompressobj(-15).decompress(d[16:])
        self.sys = body[:self.ss]; self.gfx = body[self.ss:self.ss + self.gs]
        self.raw_len = len(body)
    def loc(self, va):
        if va & 0x60000000 == 0x60000000: return self.gfx, va - 0x60000000, "G"
        if va & 0x50000000 == 0x50000000: return self.sys, va - 0x50000000, "S"
        return None, None, None
    def b(self, va, n):
        buf, off, _ = self.loc(va)
        if buf is None or va == 0 or off + n > len(buf): return None
        return buf[off:off + n]
    def u8(self, va): return self.b(va, 1)[0]
    def u16(self, va): return struct.unpack("<H", self.b(va, 2))[0]
    def u32(self, va): return struct.unpack("<I", self.b(va, 4))[0]
    def u64(self, va): return struct.unpack("<Q", self.b(va, 8))[0]
    def f32(self, va): return struct.unpack("<f", self.b(va, 4))[0]
    def s(self, va):
        buf, off, _ = self.loc(va)
        if buf is None or va == 0: return None
        e = buf.index(b"\0", off); return buf[off:e].decode("latin1")

G9FMT = {0: ("NONE", 0), 2: ("R32G32B32A32_FLOAT", 16), 6: ("R32G32B32_FLOAT", 12), 10: ("R16G16B16A16_FLOAT", 8),
         16: ("R32G32_TYPELESS", 8), 24: ("R10G10B10A2", 4), 28: ("R8G8B8A8_UNORM", 4), 30: ("R8G8B8A8_UINT", 4), 34: ("R16G16_FLOAT", 4)}
SEM = ["POSITION", "POSITION1", "POSITION2", "POSITION3", "NORMAL", "NORMAL1", "NORMAL2", "NORMAL3", "TANGENT", "TANGENT1", "TANGENT2", "TANGENT3",
       "BINORMAL", "BINORMAL1", "BINORMAL2", "BINORMAL3", "BLENDWEIGHT", "BLENDWEIGHT1", "BLENDWEIGHT2", "BLENDWEIGHT3", "BLENDINDICES", "BLENDINDICES1",
       "BLENDINDICES2", "BLENDINDICES3", "COLOR0", "COLOR1", "COLOR2", "COLOR3"] + ["TEXCOORD%d" % i for i in range(24)]
SLOT2LEG = {0: 0, 4: 3, 8: 14, 16: 1, 20: 2, 24: 4, 25: 5, 28: 6, 29: 7, 30: 8, 31: 9, 32: 10, 33: 11}
LEGNAME = ["Position", "BlendWeights", "BlendIndices", "Normal", "Colour0", "Colour1", "TexCoord0", "TexCoord1", "TexCoord2", "TexCoord3",
           "TexCoord4", "TexCoord5", "TexCoord6", "TexCoord7", "Tangent", "Binormal"]
LEGTYPE = {0: ("Nothing", 0), 1: ("Half2", 4), 2: ("Float", 4), 3: ("Half4", 8), 4: ("FloatUnk", 4), 5: ("Float2", 8), 6: ("Float3", 12),
           7: ("Float4", 16), 8: ("UByte4", 4), 9: ("Colour", 4), 10: ("RGBA8SNorm", 4)}
GTAV1 = 0x7755555555996996

def legacy_stride(flags, types=GTAV1):
    s = 0
    for i in range(16):
        if flags >> i & 1: s += LEGTYPE[(types >> (i * 4)) & 0xF][1]
    return s

class Probe:
    def __init__(self, path):
        self.r = Res(path); self.path = path
        self.gen9 = self.r.version in (171, 159, 5)  # yft171 ydr/ydd159
        self.drawables = collections.OrderedDict()
        self.decls = collections.Counter(); self.decl_info = {}
        self.geoms = []; self.shaders = {}; self.ibsizes = collections.Counter()
        self.notes = []

    # ---- fragment ----
    def walk_yft(self):
        r = self.r; ft = 0x50000000
        self.frag = dict(name=r.s(r.u64(ft + 0x58)), bs_center=struct.unpack("<3f", r.b(ft + 0x20, 12)), bs_radius=r.f32(ft + 0x2C),
                         glass_count=r.u8(ft + 0xD9), glass_ptr=r.u64(ft + 0xE0), vglass_ptr=r.u64(ft + 0x120),
                         cloth_list=(r.u64(ft + 0x60), r.u16(ft + 0x68)), drawable_cloth=r.u64(ft + 0xF8),
                         lights=(r.u64(ft + 0x110), r.u16(ft + 0x118)), bone_tf=r.u64(ft + 0xA8))
        self.add_drawable(r.u64(ft + 0x30), "frag.drawable")
        cnt = r.u32(ft + 0x48); arr = r.u64(ft + 0x38)
        for i in range(cnt if arr else 0):
            self.add_drawable(r.u64(arr + 8 * i), "frag.array[%d]" % i)
        self.add_drawable(r.u64(ft + 0xF8), "frag.cloth_drawable")
        grp = r.u64(ft + 0xF0); self.children = []
        if grp:
            for li, off in enumerate((0x10, 0x18, 0x20)):
                lod = r.u64(grp + off)
                if not lod: continue
                chp = r.u64(lod + 0xD0); chn = r.u8(lod + 0x11D)
                for ci in range(chn):
                    c = r.u64(chp + 8 * ci)
                    tag = r.u16(c + 0x12); gi = r.u16(c + 0x10)
                    self.children.append((li + 1, ci, gi, tag, r.u64(c + 0xA0), r.u64(c + 0xA8)))
                    self.add_drawable(r.u64(c + 0xA0), "lod%d.child%d.d1" % (li + 1, ci))
                    self.add_drawable(r.u64(c + 0xA8), "lod%d.child%d.d2" % (li + 1, ci))

    def add_drawable(self, va, label):
        if not va: return
        if va in self.drawables:
            self.drawables[va]["labels"].append(label); return
        d = dict(labels=[label], lods={}, shaders=[])
        self.drawables[va] = d
        r = self.r
        sg = r.u64(va + 0x10)
        if sg: d["shaders"] = self.read_shader_group(sg)
        d["skel"] = self.read_skel(r.u64(va + 0x18))
        hp = r.u64(va + 0x50) or r.u64(va + 0xA0)
        for lname, p in (("high", hp), ("med", r.u64(va + 0x58)), ("low", r.u64(va + 0x60)), ("vlow", r.u64(va + 0x68))):
            if not p: continue
            lp = r.u64(p); lc = r.u16(p + 8)
            models = []
            for mi in range(lc):
                models.append(self.read_model(r.u64(lp + 8 * mi), d, "%s.%s[%d]" % (label, lname, mi)))
            d["lods"][lname] = models

    def read_skel(self, va):
        if not va: return None
        r = self.r
        bp = r.u64(va + 0x20); n = r.u16(va + 0x5E)
        bones = []
        for i in range(n):
            b = va and bp + 80 * i
            bones.append(dict(tag=r.u16(b + 0x44), name=r.s(r.u64(b + 0x38)), parent=struct.unpack("<h", r.b(b + 0x32, 2))[0],
                              flags=r.u16(b + 0x40), t=tuple(round(x, 4) for x in struct.unpack("<3f", r.b(b + 0x10, 12)))))
        return dict(count=n, unk50=(r.u32(va + 0x50), r.u32(va + 0x54), r.u32(va + 0x58)), bones=bones)

    def read_shader_group(self, va):
        r = self.r
        sp = r.u64(va + 0x10); n = r.u16(va + 0x18)
        out = []
        for i in range(n):
            s = r.u64(sp + 8 * i)
            out.append(self.read_shader(s))
        return out

    def read_shader(self, va):
        r = self.r
        if va in self.shaders: return self.shaders[va]
        if self.gen9:
            name = r.u32(va); preset = r.u32(va + 4)
            info = r.u64(va + 0x20)
            nb, nt, nu, ns, npar, u0, u1, u2 = r.b(info, 8)
            params = []
            for i in range(npar):
                h, data = struct.unpack("<II", r.b(info + 8 + 8 * i, 8))
                t = data & 3
                params.append(dict(hash=h, name=nm(h), type=["Texture", "Unknown", "Sampler", "CBuffer"][t],
                                   idx=(data >> 2) & 0xFF, off=(data >> 8) & 0xFFF, len=(data >> 20) & 0xFFF))
            sh = dict(va=va, name_hash=name, name=nm(name), preset=preset, preset_name=nm(preset), bucket=r.u8(va + 0x39),
                      pdsize=r.u16(va + 0x3A), mask=r.u32(va + 0x3C), u38=r.u8(va + 0x38), unk18=r.u64(va + 0x18),
                      u28=r.u64(va + 0x28), u30=r.u64(va + 0x30), counts=(nb, nt, nu, ns, npar, u0, u1, u2), params=params)
        else:
            pp = r.u64(va); name = r.u32(va + 8); cnt = r.u8(va + 0x10)
            offs = 0; types = []
            for i in range(cnt):
                dt = r.u8(pp + 16 * i); types.append(dt)
                offs += 0 if dt == 0 else 16 * dt
            hp = pp + 16 * cnt + offs
            params = []
            for i in range(cnt):
                h = r.u32(hp + 4 * i)
                params.append(dict(hash=h, name=nm(h), type="Texture" if types[i] == 0 else "Vec4x%d" % types[i]))
            sh = dict(va=va, name_hash=name, name=nm(name), file=nm(r.u32(va + 0x18)), bucket=r.u8(va + 0x11),
                      psize=r.u16(va + 0x14), pdsize=r.u16(va + 0x16), texcount=r.u8(va + 0x27), params=params)
        self.shaders[va] = sh
        return sh

    def read_model(self, va, d, label):
        r = self.r
        gp = r.u64(va + 8); gc = r.u16(va + 0x10); smp = r.u64(va + 0x20)
        sb = r.u32(va + 0x28)
        m = dict(label=label, geoms=[], skelbind=sb, rendermask=r.u16(va + 0x2C))
        for gi in range(gc):
            g = r.u64(gp + 8 * gi)
            sid = r.u16(smp + 2 * gi)
            m["geoms"].append(self.read_geom(g, sid, d, "%s.g%d" % (label, gi)))
        return m

    def read_geom(self, va, sid, d, label):
        r = self.r
        vb = r.u64(va + 0x18); ib = r.u64(va + 0x38)
        g = dict(label=label, shader_id=sid, icount=r.u32(va + 0x58), tris=r.u32(va + 0x5C), vcount16=r.u16(va + 0x60),
                 prim=r.u16(va + 0x62), stride_geom=r.u16(va + 0x70), boneids=r.u16(va + 0x72))
        sh = d["shaders"][sid] if sid < len(d["shaders"]) else None
        g["shader"] = sh["name"] if sh else None
        if self.gen9:
            vcount = r.u32(vb + 8); stride = r.u16(vb + 0xC)
            g.update(vb_vcount=vcount, vb_stride=stride, vb_unkE=r.u16(vb + 0xE), vb_bind=r.u32(vb + 0x10), vb_unk14=r.u32(vb + 0x14),
                     vb_data=r.u64(vb + 0x18), vb_pad=(r.u64(vb + 0x20), r.u64(vb + 0x28)), vb_srv=r.u64(vb + 0x30))
            dp = r.u64(vb + 0x38)
            raw = r.b(dp, 320)
            offs = struct.unpack_from("<52I", raw, 0); sizes = raw[208:260]; types = raw[260:312]; data = struct.unpack_from("<Q", raw, 312)[0]
            elems = tuple((i, types[i], offs[i]) for i in range(52) if types[i])
            key = ("g9", elems, (data >> 2) & 0xFF, data & 3)
            if key not in self.decl_info:
                legflags = 0; dropped = []
                for i, t, o in elems:
                    if i in SLOT2LEG: legflags |= 1 << SLOT2LEG[i]
                    else: dropped.append(SEM[i])
                self.decl_info[key] = dict(elems=[(SEM[i], G9FMT.get(t, ("?%d" % t, 0))[0], o, G9FMT.get(t, ("?", 0))[1]) for i, t, o in elems],
                                           vsize=(data >> 2) & 0xFF, soa=data & 1, flag=(data >> 1) & 1, dcount=(data >> 10) & 0x3FFFFF,
                                           sizes_distinct=sorted(set(sizes[i] for i, _, _ in elems)), data_hi=data >> 32,
                                           leg_flags=legflags, leg_stride_gtav1=legacy_stride(legflags), dropped=dropped,
                                           sum_elem=sum(G9FMT.get(t, ("?", 0))[1] for _, t, _ in elems), decl_va=set())
            self.decl_info[key]["decl_va"].add(dp)
            self.decls[key] += 1
            g["decl"] = key
            # check vertex data location + sample half floats
            buf, off, sec = r.loc(g["vb_data"]); g["vb_sec"] = sec
            # IB
            ic = r.u32(ib + 8); isz = r.u16(ib + 0xC)
            g.update(ib_count=ic, ib_size=isz, ib_unkE=r.u16(ib + 0xE), ib_bind=r.u32(ib + 0x10), ib_ptr=r.u64(ib + 0x18), ib_srv=r.u64(ib + 0x30))
            self.ibsizes[isz] += 1
            idx = r.b(g["ib_ptr"], ic * isz)
            if idx is None: self.notes.append("%s: index data out of range" % label)
            else:
                fmt = "<%d%s" % (ic, "H" if isz == 2 else "I")
                ii = struct.unpack(fmt, idx); g["max_index"] = max(ii) if ii else -1
            g["ib_sec"] = r.loc(g["ib_ptr"])[2]
        else:
            stride = r.u16(vb + 8); vcount = r.u32(vb + 0x18)
            dp = r.u64(vb + 0x30)
            flags, dstride, du, dcount, dtypes = struct.unpack("<IHBBQ", r.b(dp, 16))
            key = ("leg", flags, dtypes, dstride)
            if key not in self.decl_info:
                elems = [(LEGNAME[i], LEGTYPE[(dtypes >> (4 * i)) & 0xF][0]) for i in range(16) if flags >> i & 1]
                self.decl_info[key] = dict(elems=elems, flags=flags, types=hex(dtypes), stride=dstride, count=dcount, decl_va=set())
            self.decl_info[key]["decl_va"].add(dp)
            self.decls[key] += 1
            g.update(vb_vcount=vcount, vb_stride=stride, vb_flags=r.u16(vb + 0xA), vb_data=r.u64(vb + 0x10), vb_data2=r.u64(vb + 0x20), decl=key)
            g["vb_sec"] = r.loc(g["vb_data"])[2]
            ic = r.u32(ib + 8); g.update(ib_count=ic, ib_size=2, ib_ptr=r.u64(ib + 0x10)); self.ibsizes[2] += 1
            g["ib_sec"] = r.loc(g["ib_ptr"])[2]
            idx = r.b(g["ib_ptr"], ic * 2)
            if idx: g["max_index"] = max(struct.unpack("<%dH" % ic, idx))
        self.geoms.append(g)
        return g

def main():
    path = sys.argv[1]
    p = Probe(path)
    p.walk_yft()
    r = p.r
    print("FILE", path)
    print("version %d hdrver %d gen9=%s sys=%d gfx=%d inflated=%d" % (r.version, r.hdr_ver, p.gen9, r.ss, r.gs, r.raw_len))
    print("frag", {k: v for k, v in p.frag.items()})
    print("drawables:", len(p.drawables), " geometries:", len(p.geoms))
    for va, d in p.drawables.items():
        lods = {k: (len(v), sum(len(m["geoms"]) for m in v)) for k, v in d["lods"].items()}
        print("  0x%X %s shaders=%d skel=%s lods(models,geoms)=%s" % (va, d["labels"][:3], len(d["shaders"]), d["skel"]["count"] if d["skel"] else None, lods))
    print("\nDECLARATIONS (distinct):")
    for key, n in p.decls.most_common():
        di = p.decl_info[key]
        if key[0] == "g9":
            print(" [%d geoms] vsize=%d soa=%d flag=%d dcount=%d sizesfield=%s sum_elem=%d data_hi=%d decl_blocks=%d" % (n, di["vsize"], di["soa"], di["flag"], di["dcount"], di["sizes_distinct"], di["sum_elem"], di["data_hi"], len(di["decl_va"])))
            print("     elems:", ", ".join("%s:%s@%d" % (s, f, o) for s, f, o, _ in di["elems"]))
            print("     -> legacy flags 0x%X (%s) GTAV1 stride=%d dropped=%s" % (di["leg_flags"], "+".join(LEGNAME[i] for i in range(16) if di["leg_flags"] >> i & 1), di["leg_stride_gtav1"], di["dropped"]))
        else:
            print(" [%d geoms] flags=0x%X types=%s stride=%d count=%d decl_blocks=%d" % (n, di["flags"], di["types"], di["stride"], di["count"], len(di["decl_va"])))
            print("     elems:", ", ".join("%s:%s" % e for e in di["elems"]))
    print("\nINDEX sizes:", dict(p.ibsizes))
    big = [g for g in p.geoms if g["vb_vcount"] > 65535]
    print("geoms with >65535 verts:", len(big), " max vcount:", max(g["vb_vcount"] for g in p.geoms), " max index:", max(g.get("max_index", 0) for g in p.geoms))
    mism = [g["label"] for g in p.geoms if g["vb_vcount"] != g["vcount16"]]
    print("vb_vcount != geom.VerticesCount(u16):", len(mism))
    secs = collections.Counter((g["vb_sec"], g["ib_sec"]) for g in p.geoms); print("vertex/index data sections:", dict(secs))
    if p.gen9:
        print("vb bindflags:", collections.Counter(hex(g["vb_bind"]) for g in p.geoms), " unkE:", collections.Counter(g["vb_unkE"] for g in p.geoms),
              " unk14:", collections.Counter(g["vb_unk14"] for g in p.geoms), " pads nonzero:", sum(1 for g in p.geoms if any(g["vb_pad"])), " srv nonzero:", sum(1 for g in p.geoms if g["vb_srv"]))
        print("ib bindflags:", collections.Counter(hex(g["ib_bind"]) for g in p.geoms), " ib srv nonzero:", sum(1 for g in p.geoms if g["ib_srv"]))
        print("geom stride field == vb stride:", sum(1 for g in p.geoms if g["stride_geom"] == g["vb_stride"]), "/", len(p.geoms))
    print("\nSHADERS (distinct by name):")
    byname = collections.OrderedDict()
    for sh in p.shaders.values():
        byname.setdefault(sh["name"], []).append(sh)
    for name, shs in byname.items():
        sh = shs[0]
        if p.gen9:
            conv = CONV.get(sh["name_hash"])
            presets = sorted(set(s["preset_name"] for s in shs))
            print(" %-28s x%-2d presets=%s buckets=%s inXML=%s counts=%s u38=%s unk18nz=%d" % (name, len(shs), presets, sorted(set(s["bucket"] for s in shs)), conv is not None,
                  sh["counts"], sorted(set(s["u38"] for s in shs)), sum(1 for s in shs if s["unk18"])))
            psets = set(tuple(pp["hash"] for pp in s["params"]) for s in shs)
            if len(psets) > 1: print("     NOTE: %d distinct param sets among instances" % len(psets))
            nomap = []; notinxml = []
            for pp in sh["params"]:
                if conv is None: break
                c = conv["params"].get(pp["hash"])
                if c is None: notinxml.append(pp["name"])
                elif not c[2] and pp["type"] in ("Texture", "CBuffer"): nomap.append("%s(%s)" % (c[1], pp["type"]))
            print("     params:", ", ".join("%s/%s" % (pp["name"], pp["type"][0]) for pp in sh["params"]))
            if conv is not None:
                print("     tex/cbuf params w/o 'old':", nomap)
                print("     params not in XML entry:", notinxml)
                xmlonly = [v[1] for h, v in conv["params"].items() if h not in set(pp["hash"] for pp in sh["params"])]
                if xmlonly: print("     XML params absent in file:", xmlonly)
        else:
            print(" %-28s x%-2d file=%s buckets=%s inXML=%s" % (name, len(shs), sh["file"], sorted(set(s["bucket"] for s in shs)), sh["name_hash"] in CONV))
            print("     params:", ", ".join("%s/%s" % (pp["name"], pp["type"]) for pp in sh["params"]))
    if p.notes: print("NOTES:", p.notes)
    if "--json" in sys.argv:
        out = sys.argv[sys.argv.index("--json") + 1]
        def conv_(o):
            if isinstance(o, set): return sorted(o)
            if isinstance(o, tuple): return list(o)
            return str(o)
        dump = dict(version=r.version, children=p.children, skels=[d["skel"] for d in p.drawables.values()],
                    geoms=[{k: (str(v) if k == "decl" else v) for k, v in g.items()} for g in p.geoms],
                    shaders=[{k: v for k, v in s.items()} for s in p.shaders.values()])
        json.dump(dump, open(out, "w"), default=conv_, indent=1)

if __name__ == "__main__":
    main()
