"""Harvest legacy shader param lists (ordered) from a folder of legacy .yft, then check
every Gen9 shader in the given gen9 files: does a legacy reference exist, and is the
legacy list covered by the Gen9 params' XML 'old' names?"""
import sys, os, glob, collections, json
from g9probe import Probe, CONV, nm, joaat
folder = sys.argv[1]; g9files = sys.argv[2:]
leg = collections.defaultdict(collections.Counter); errs = 0; files = 0
for f in glob.glob(os.path.join(folder, "**", "*.yft"), recursive=True):
    try:
        p = Probe(f)
        if p.gen9 or p.r.version != 162: continue
        p.walk_yft(); files += 1
        for s in p.shaders.values():
            leg[s["name"]][tuple((pp["hash"], pp["type"]) for pp in s["params"])] += 1
    except Exception as e:
        errs += 1
print("legacy files scanned:", files, "errors:", errs, "distinct shaders:", len(leg))
multi = {k: len(v) for k, v in leg.items() if len(v) > 1}
print("shaders whose ordered param list varies between legacy instances:", multi)
json.dump({k: [[[nm(h), t] for h, t in lst] for lst in v] for k, v in leg.items()}, open("legacy_shader_params.json", "w"), indent=0)
seen = set()
for gf in g9files:
    p = Probe(gf); p.walk_yft()
    for s in p.shaders.values():
        if s["name"] in seen: continue
        seen.add(s["name"])
        conv = CONV.get(s["name_hash"])
        mapped = set()
        for pp in s["params"]:
            c = conv["params"].get(pp["hash"]) if conv else None
            if c and c[2]: mapped.add(joaat(c[2]))
        refs = leg.get(s["name"])
        if not refs:
            print(" %-28s NO legacy reference in scan" % s["name"]); continue
        lst = refs.most_common(1)[0][0]
        miss = [nm(h) for h, t in lst if h not in mapped]
        extra = sorted(nm(h) for h in mapped - set(h for h, t in lst))
        print(" %-28s legacy refs=%d params=%d  legacy params NOT obtainable from g9=%s  g9-mapped but absent in legacy=%s" % (s["name"], sum(refs.values()), len(lst), miss, extra))
