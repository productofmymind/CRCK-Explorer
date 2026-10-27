"""Bulk scan of Gen9 .yft/.ydr/.ydd: element formats per slot, dropped slots, index sizes,
big geometries, shaders vs ShadersGen9Conversion.xml and vs the harvested legacy table."""
import sys, os, glob, collections, json, struct, traceback
from g9probe import Probe, CONV, nm, joaat, SEM, G9FMT, SLOT2LEG
leg = json.load(open("legacy_shader_params.json"))
def walk(p, f):
    r = p.r
    if f.endswith(".yft"): p.walk_yft(); return
    if f.endswith(".ydr"): p.frag = {}; p.children = []; p.add_drawable(0x50000000, "ydr"); return
    if f.endswith(".ydd"):
        p.frag = {}; p.children = []
        dp = r.u64(0x50000000 + 0x30); n = r.u16(0x50000000 + 0x38)
        for i in range(n): p.add_drawable(r.u64(dp + 8 * i), "ydd[%d]" % i)
fmt = collections.Counter(); dropped = collections.Counter(); idx = collections.Counter(); vers = collections.Counter()
shaders = collections.Counter(); notxml = collections.Counter(); noleg = collections.Counter(); big = []; errs = []; nfiles = 0; ngeoms = 0
counts_u2 = collections.Counter(); presets = collections.Counter(); glass = collections.Counter(); cloth = 0; lights = 0; sizesmismatch = 0
nounk = collections.Counter(); g9only_params = collections.Counter()
for root in sys.argv[1:]:
    for f in glob.glob(os.path.join(root, "**", "*.*"), recursive=True):
        if not f.endswith((".yft", ".ydr", ".ydd")): continue
        try:
            p = Probe(f); vers[(os.path.splitext(f)[1], p.r.version)] += 1
            if not p.gen9: continue
            walk(p, f); nfiles += 1
        except Exception as e:
            errs.append((f, repr(e))); continue
        if f.endswith(".yft"):
            glass[p.frag["glass_count"]] += 1
            cloth += 1 if p.frag["cloth_list"][1] or p.frag["drawable_cloth"] else 0
            lights += 1 if p.frag["lights"][1] else 0
        for key, n in p.decls.items():
            di = p.decl_info[key]
            for s, fm, o, sz in di["elems"]: fmt[(s, fm)] += n
            for s in di["dropped"]: dropped[s] += n
            if di["vsize"] != di["sum_elem"]: sizesmismatch += n
        for g in p.geoms:
            ngeoms += 1; idx[g["ib_size"]] += 1
            if g["vb_vcount"] > 65535: big.append((f, g["label"], g["vb_vcount"]))
        for s in p.shaders.values():
            shaders[s["name"]] += 1; counts_u2[s["counts"][7]] += 1; presets[nm(s["preset"])] += 1
            nounk[s["counts"][2]] += 1
            if s["name_hash"] not in CONV: notxml[s["name"]] += 1
            if s["name"] not in leg: noleg[s["name"]] += 1
            conv = CONV.get(s["name_hash"])
            if conv:
                for pp in s["params"]:
                    c = conv["params"].get(pp["hash"])
                    if c is None: g9only_params[(s["name"], pp["name"], "NOT IN XML")] += 1
print("versions:", dict(vers)); print("gen9 files walked:", nfiles, "geometries:", ngeoms, "errors:", len(errs))
for e in errs[:10]: print("  ERR", e)
print("element formats (semantic, format): geoms"); [print("   ", k, v) for k, v in sorted(fmt.items())]
print("dropped slots:", dict(dropped)); print("index sizes:", dict(idx)); print(">65535-vertex geoms:", len(big), big[:5])
print("decl VertexSize != sum(element sizes):", sizesmismatch)
print("ShaderParamInfos.Unknown2 values:", dict(counts_u2), " NumUnknowns:", dict(nounk), " presets:", dict(presets))
print("yft glass window counts:", dict(glass), " yft with cloth:", cloth, " yft with lights:", lights)
print("distinct shaders:", len(shaders)); print("  not in ShadersGen9Conversion.xml:", dict(notxml))
print("  no legacy reference in harvested legacy table:", dict(noleg))
print("  gen9 params absent from XML entry:", dict(g9only_params))
print("all shader names:", dict(shaders))
