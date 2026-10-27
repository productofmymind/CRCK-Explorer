"""Throwaway oracle check: same vanilla .yft in Enhanced (Gen9) vs Legacy.
usage: python g9compare.py <gen9.yft> <legacy.yft>
Reorders each Gen9 interleaved vertex into the legacy GTAV1 element order and
byte-compares it with the legacy vertex; compares indices, shaders (via the
The reference tool ShadersGen9Conversion.xml 'old' names), CBuffer values, textures,
skeleton.
"""
import sys, struct, collections
from g9probe import Probe, CONV, nm, joaat, SLOT2LEG, G9FMT, LEGTYPE, GTAV1

def leg_offsets(flags, types=GTAV1):
    o = {}; off = 0
    for i in range(16):
        if flags >> i & 1:
            o[i] = off; off += LEGTYPE[(types >> (4 * i)) & 0xF][1]
    return o, off

def g9_decl(r, geom_vb):
    dp = r.u64(geom_vb + 0x38); raw = r.b(dp, 320)
    offs = struct.unpack_from("<52I", raw, 0); types = raw[260:312]
    return [(i, types[i], offs[i]) for i in range(52) if types[i]]

def main():
    g9 = Probe(sys.argv[1]); g9.walk_yft()
    lg = Probe(sys.argv[2]); lg.walk_yft()
    assert g9.gen9 and not lg.gen9
    print("geoms gen9=%d legacy=%d" % (len(g9.geoms), len(lg.geoms)))
    tot = collections.Counter()
    elem_diff = collections.Counter(); elem_tot = collections.Counter(); maxabs = collections.defaultdict(float)
    for a, b in zip(g9.geoms, lg.geoms):
        r9, rl = g9.r, lg.r
        if a["vb_vcount"] != b["vb_vcount"] or a["ib_count"] != b["ib_count"]:
            tot["count_mismatch"] += 1; print(" count mismatch", a["label"], a["vb_vcount"], b["vb_vcount"], a["ib_count"], b["ib_count"]); continue
        # decl
        vb9 = None
        # re-find vb pointer from geometry label: stash in probe? recompute via vb_data -> need decl ptr; read from geometry struct again
        elems = list(a["decl"][1])
        legflags = 0
        for i, t, o in elems:
            legflags |= 1 << SLOT2LEG[i]
        ldi = lg.decl_info[b["decl"]]
        if legflags != ldi["flags"] or ldi["types"] != hex(GTAV1):
            tot["decl_mismatch"] += 1; print(" decl mismatch", a["label"], hex(legflags), ldi["flags"], ldi["types"]); continue
        loffs, lstride = leg_offsets(legflags)
        assert lstride == b["vb_stride"] == a["vb_stride"], (lstride, b["vb_stride"], a["vb_stride"])
        n = a["vb_vcount"]; st = a["vb_stride"]
        d9 = r9.b(a["vb_data"], n * st); dl = rl.b(b["vb_data"], n * st)
        out = bytearray(n * st)
        for i, t, o in elems:
            sz = G9FMT[t][1]; lo = loffs[SLOT2LEG[i]]
            for v in range(n):
                out[v * st + lo: v * st + lo + sz] = d9[v * st + o: v * st + o + sz]
        if bytes(out) == dl: tot["vb_identical"] += 1
        else:
            tot["vb_differs"] += 1
            for i, t, o in elems:
                sz = G9FMT[t][1]; lo = loffs[SLOT2LEG[i]]; key = "slot%d/%s" % (i, G9FMT[t][0])
                for v in range(n):
                    x = out[v * st + lo: v * st + lo + sz]; y = dl[v * st + lo: v * st + lo + sz]
                    elem_tot[key] += 1
                    if x != y:
                        elem_diff[key] += 1
                        if G9FMT[t][0].startswith("R32"):
                            fa = struct.unpack("<%df" % (sz // 4), x); fb = struct.unpack("<%df" % (sz // 4), y)
                            maxabs[key] = max(maxabs[key], max(abs(p - q) for p, q in zip(fa, fb)))
                        else:
                            maxabs[key] = max(maxabs[key], max(abs(p - q) for p, q in zip(x, y)))
        i9 = r9.b(a["ib_ptr"], a["ib_count"] * 2); il = rl.b(b["ib_ptr"], b["ib_count"] * 2)
        tot["ib_identical" if i9 == il else "ib_differs"] += 1
        if a["shader"] != b["shader"]: tot["shader_name_mismatch"] += 1
    print("geometry results:", dict(tot))
    if elem_diff:
        print("per-element differing vertices (diff/total, max abs diff):")
        for k in sorted(elem_tot):
            print("   %-32s %6d/%-6d  max %s" % (k, elem_diff[k], elem_tot[k], maxabs.get(k)))

    # shaders, index-aligned within main drawable
    s9 = list(g9.shaders.values()); sl = list(lg.shaders.values())
    print("\nshaders gen9=%d legacy=%d" % (len(s9), len(sl)))
    r9, rl = g9.r, lg.r
    for a, b in zip(s9, sl):
        conv = CONV.get(a["name_hash"])
        mapped = {}
        unmapped = []
        for pp in a["params"]:
            c = conv["params"].get(pp["hash"]) if conv else None
            if c and c[2]: mapped[joaat(c[2])] = pp
            elif pp["type"] in ("Texture", "CBuffer"): unmapped.append(pp["name"])
        legset = [pp["hash"] for pp in b["params"]]
        missing = [nm(h) for h in legset if h not in mapped]
        extra = [nm(h) for h in mapped if h not in legset]
        # values
        bufptrs_va = r9.u64(a["va"] + 8)
        nb = a["counts"][0]
        bufptrs = [r9.u64(bufptrs_va + 8 * i) for i in range(nb + 1)] if nb else []
        texptrs_va = r9.u64(a["va"] + 0x10)
        pp_l = r9  # noqa
        lpp = rl.u64(b["va"]); vdiff = []; tdiff = []; nval = 0; ntex = 0
        for idx, h in enumerate(legset):
            g = mapped.get(h)
            if not g: continue
            dt = rl.u8(lpp + 16 * idx); dptr = rl.u64(lpp + 16 * idx + 8)
            if g["type"] == "Texture":
                ntex += 1
                tl = rl.s(rl.u64(dptr + 0x28)) if dptr else None
                tp = r9.u64(texptrs_va + 8 * g["idx"])
                t9 = r9.s(r9.u64(tp + 0x28)) if tp else None
                if (tl or "").lower() != (t9 or "").lower(): tdiff.append((nm(h), t9, tl))
            else:
                nval += 1
                base = bufptrs[g["idx"] & 0x3F]
                raw9 = r9.b(base + g["off"], g["len"])
                rawl = rl.b(dptr, 16 * max(dt, 1))
                f9 = struct.unpack("<%df" % (g["len"] // 4), raw9)
                fl = struct.unpack("<%df" % (len(rawl) // 4), rawl)[:len(f9)]
                if any(abs(x - y) > 1e-6 for x, y in zip(f9, fl)): vdiff.append((nm(h), f9[:4], fl[:4]))
        print(" %-28s legacy %-28s legparams=%d mapped=%d missing_in_g9=%s extra_from_g9=%s g9_unmapped_tex/cbuf=%s" % (a["name"], b["name"], len(legset), len(mapped), missing, extra, unmapped))
        print("     values compared=%d differ=%s | textures compared=%d differ=%s | bucket %d/%d" % (nval, vdiff, ntex, tdiff, a["bucket"], b["bucket"]))

    # skeleton
    k9 = next(iter(g9.drawables.values()))["skel"]; kl = next(iter(lg.drawables.values()))["skel"]
    print("\nskeleton bones gen9=%d legacy=%d unk50 %s / %s" % (k9["count"], kl["count"], k9["unk50"], kl["unk50"]))
    diffs = [(x["name"], x["tag"], y["name"], y["tag"], x["t"], y["t"]) for x, y in zip(k9["bones"], kl["bones"]) if (x["tag"], x["name"], x["parent"], x["t"], x["flags"]) != (y["tag"], y["name"], y["parent"], y["t"], y["flags"])]
    print("bone diffs:", len(diffs), diffs[:10])
    print("children gen9=%d legacy=%d" % (len(g9.children), len(lg.children)))
    cd = [(x, y) for x, y in zip(g9.children, lg.children) if x[:4] != y[:4]]
    print("child (lod,idx,group,tag) diffs:", cd[:5])

if __name__ == "__main__":
    main()
