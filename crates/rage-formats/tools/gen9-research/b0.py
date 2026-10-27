import sys,glob,os,collections
sys.path.insert(0,'.')
from fragwalk import R
for ed in ('leg','enh'):
    c=collections.Counter(); u12=collections.Counter()
    for p in glob.glob('all/%s/levels/gta5/vehicles.rpf/*.yft'%ed):
        r=R(p); ft=0x50000000; c[r.u32(ft+0xB0)]+=1
        bt=r.u64(ft+0xA8); u12[r.u16(bt+0x12)]+=1
    print(ed,'B0 values (top):',c.most_common(6),'BoneTransforms.12h:',dict(u12))
