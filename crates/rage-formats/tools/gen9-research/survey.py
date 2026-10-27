import sys, glob, collections
sys.path.insert(0,'.')
from fragwalk import R
C=collections.Counter(); ex=collections.defaultdict(list); b0=collections.Counter()
for p in glob.glob(sys.argv[1]):
    try: r=R(p)
    except Exception as e: C['err']+=1; continue
    ft=0x50000000; import os; n=os.path.basename(p)
    C['files']+=1
    def f(key,cond):
        if cond: C[key]+=1; ex[key].append(n) if len(ex[key])<5 else None
    f('glass_windows', r.u8(ft+0xD9)); f('cloths', r.u16(ft+0x68)); f('drawable_array', r.u32(ft+0x48))
    f('lights', r.u16(ft+0x118)); f('vgw', r.u64(ft+0x120)); f('drawable_cloth', r.u64(ft+0xF8)); f('bone_transforms', r.u64(ft+0xA8))
    f('gfx_section_nonempty', r.gs>0)
    b0[r.u32(ft+0xB0)!=0]+=1
    plg=r.u64(ft+0xF0)
    if plg:
        for i in range(3):
            lod=r.u64(plg+0x10+8*i)
            if not lod: continue
            f('LOD%d'%(i+1),1)
            if i==0:
                f('artbody', r.u64(lod+0x20)); f('ud1', r.u8(lod+0x118)); f('ud2', r.u8(lod+0x119)); f('arch2', r.u64(lod+0xE0))
                cp=r.u64(lod+0xD0)
                d2=0
                for c in range(r.u8(lod+0x11D)):
                    if r.u64(r.u64(cp+8*c)+0xA8): d2=1
                f('child_drawable2', d2)
    d=r.u64(ft+0x30); f('fragmatrices', r.u16(d+0x110))
print(dict(C)); print('B0 nonzero:',dict(b0))
for k,v in ex.items(): print(k, v)
