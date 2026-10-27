"""Pairwise oracle over two folders holding the same relative .yft paths (Gen9 vs legacy).
Per geometry: decl mapping matches legacy flags/types/stride; reordered VB byte-identical?;
if not, same vertex multiset and same triangle set?; IB identical?. Per shader: legacy params covered
by Gen9 'old' names, CBuffer values and texture names equal. Skeleton equal."""
import sys, os, glob, struct, collections
from g9probe import Probe, CONV, nm, joaat, SLOT2LEG, G9FMT, GTAV1
from g9compare import leg_offsets
g9root, lroot = sys.argv[1], sys.argv[2]
T = collections.Counter(); bad = []
for f9 in glob.glob(os.path.join(g9root, "**", "*.yft"), recursive=True):
    rel = os.path.relpath(f9, g9root); fl = os.path.join(lroot, rel)
    if not os.path.exists(fl): T["no_legacy_pair"] += 1; continue
    try:
        a = Probe(f9); a.walk_yft(); b = Probe(fl); b.walk_yft()
    except Exception as e:
        T["parse_error"] += 1; bad.append((rel, repr(e))); continue
    T["pairs"] += 1
    if len(a.geoms) != len(b.geoms): T["geomcount_mismatch"] += 1; bad.append((rel, "geoms %d/%d" % (len(a.geoms), len(b.geoms)))); continue
    for x, y in zip(a.geoms, b.geoms):
        T["geoms"] += 1
        if x["vb_vcount"] != y["vb_vcount"] or x["ib_count"] != y["ib_count"]: T["g_count_mismatch"] += 1; bad.append((rel, x["label"], "counts v%d/%d i%d/%d" % (x["vb_vcount"], y["vb_vcount"], x["ib_count"], y["ib_count"]))); continue
        flags = 0
        for i, t, o in x["decl"][1]: flags |= 1 << SLOT2LEG[i]
        ld = b.decl_info[y["decl"]]
        if flags != ld["flags"] or ld["types"] != hex(GTAV1): T["g_decl_mismatch"] += 1; bad.append((rel, x["label"], hex(flags), hex(ld["flags"]), ld["types"])); continue
        lo, st = leg_offsets(flags); n = x["vb_vcount"]
        if st != y["vb_stride"]: T["g_stride_mismatch"] += 1; continue
        d9 = a.r.b(x["vb_data"], n * st); dl = b.r.b(y["vb_data"], n * st)
        out = bytearray(n * st)
        for i, t, o in x["decl"][1]:
            sz = G9FMT[t][1]; l = lo[SLOT2LEG[i]]
            for v in range(n): out[v * st + l:v * st + l + sz] = d9[v * st + o:v * st + o + sz]
        i9 = a.r.b(x["ib_ptr"], x["ib_count"] * 2); il = b.r.b(y["ib_ptr"], y["ib_count"] * 2)
        if bytes(out) == dl and i9 == il: T["g_identical"] += 1; continue
        v9 = [bytes(out[v * st:(v + 1) * st]) for v in range(n)]; vl = [dl[v * st:(v + 1) * st] for v in range(n)]
        I9 = struct.unpack("<%dH" % x["ib_count"], i9); IL = struct.unpack("<%dH" % y["ib_count"], il)
        t9 = sorted(tuple(sorted((v9[I9[k]], v9[I9[k + 1]], v9[I9[k + 2]]))) for k in range(0, len(I9), 3))
        tl = sorted(tuple(sorted((vl[IL[k]], vl[IL[k + 1]], vl[IL[k + 2]]))) for k in range(0, len(IL), 3))
        if t9 == tl: T["g_reordered_same_triangles"] += 1
        else: T["g_geometry_differs"] += 1; bad.append((rel, x["label"], "triangles differ"))
    s9 = list(a.shaders.values()); sl = list(b.shaders.values())
    if [s["name"] for s in s9] != [s["name"] for s in sl]: T["shader_list_mismatch"] += 1; bad.append((rel, "shaders")); continue
    for sa, sb in zip(s9, sl):
        T["shaders"] += 1
        conv = CONV.get(sa["name_hash"]); mapped = {}
        for pp in sa["params"]:
            c = conv["params"].get(pp["hash"]) if conv else None
            if c and c[2]: mapped[joaat(c[2])] = pp
        bufva = a.r.u64(sa["va"] + 8); nb = sa["counts"][0]
        bufs = [a.r.u64(bufva + 8 * i) for i in range(nb + 1)]
        texva = a.r.u64(sa["va"] + 0x10); lpp = b.r.u64(sb["va"]); ok = True
        for idx, pp in enumerate(sb["params"]):
            g = mapped.get(pp["hash"])
            if not g: T["leg_param_not_from_g9"] += 1; ok = False; bad.append((rel, sa["name"], pp["name"])); continue
            dt = b.r.u8(lpp + 16 * idx); dptr = b.r.u64(lpp + 16 * idx + 8)
            if g["type"] == "Texture":
                tl_ = b.r.s(b.r.u64(dptr + 0x28)) if dptr else None
                tp = a.r.u64(texva + 8 * g["idx"]); t9_ = a.r.s(a.r.u64(tp + 0x28)) if tp else None
                if (tl_ or "").lower() != (t9_ or "").lower(): T["tex_name_diff"] += 1; ok = False
            else:
                raw9 = a.r.b(bufs[g["idx"] & 0x3F] + g["off"], g["len"]); rawl = b.r.b(dptr, 16 * max(dt, 1))
                f9 = struct.unpack("<%df" % (g["len"] // 4), raw9); fl_ = struct.unpack("<%df" % (len(rawl) // 4), rawl)[:len(f9)]
                if any(abs(p - q) > 1e-5 for p, q in zip(f9, fl_)): T["cbuf_value_diff"] += 1; ok = False; bad.append((rel, sa["name"], pp["name"], f9[:4], fl_[:4]))
        if ok: T["shader_ok"] += 1
    k9 = next(iter(a.drawables.values()))["skel"]; kl = next(iter(b.drawables.values()))["skel"]
    if (k9 and kl) and [(x["tag"], x["name"], x["parent"], x["t"], x["flags"]) for x in k9["bones"]] != [(x["tag"], x["name"], x["parent"], x["t"], x["flags"]) for x in kl["bones"]]: T["skeleton_diff"] += 1
print(dict(T)); print("issues (first 25):"); [print("  ", x) for x in bad[:25]]
c = collections.Counter(x[-1] if len(x) == 3 and isinstance(x[-1], str) else None for x in bad)
