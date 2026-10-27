"""List differing geometries between Gen9 and legacy, test multiset equality and triangle-set equality."""
import sys, struct
from g9probe import Probe, SLOT2LEG, G9FMT, GTAV1
from g9compare import leg_offsets
g9 = Probe(sys.argv[1]); g9.walk_yft(); lg = Probe(sys.argv[2]); lg.walk_yft()
for a, b in zip(g9.geoms, lg.geoms):
    elems = list(a["decl"][1]); flags = 0
    for i,t,o in elems: flags |= 1 << SLOT2LEG[i]
    lo, st = leg_offsets(flags); n = a["vb_vcount"]
    d9 = g9.r.b(a["vb_data"], n*st); dl = lg.r.b(b["vb_data"], n*st)
    out = bytearray(n*st)
    for i,t,o in elems:
        sz = G9FMT[t][1]; l = lo[SLOT2LEG[i]]
        for v in range(n): out[v*st+l:v*st+l+sz] = d9[v*st+o:v*st+o+sz]
    v9 = [bytes(out[v*st:(v+1)*st]) for v in range(n)]; vl = [dl[v*st:(v+1)*st] for v in range(n)]
    i9 = struct.unpack("<%dH"%a["ib_count"], g9.r.b(a["ib_ptr"], a["ib_count"]*2)); il = struct.unpack("<%dH"%b["ib_count"], lg.r.b(b["ib_ptr"], b["ib_count"]*2))
    if v9 == vl and i9 == il: continue
    tri9 = sorted(tuple(sorted((v9[i9[k]], v9[i9[k+1]], v9[i9[k+2]]))) for k in range(0, len(i9), 3))
    tril = sorted(tuple(sorted((vl[il[k]], vl[il[k+1]], vl[il[k+2]]))) for k in range(0, len(il), 3))
    ndiffv = sum(1 for x,y in zip(v9,vl) if x!=y)
    print("%-28s %-24s v=%5d i=%5d vtx_diff=%4d vb_multiset_eq=%s ib_eq=%s triangle_set_eq=%s" % (a["label"], a["shader"], n, a["ib_count"], ndiffv, sorted(v9)==sorted(vl), i9==il, tri9==tril))
