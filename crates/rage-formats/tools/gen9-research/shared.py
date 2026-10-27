import sys; sys.path.insert(0,'.')
from fragwalk import R
for p in sys.argv[1:]:
    r=R(p); ft=0x50000000; d=r.u64(ft+0x30); plg=r.u64(ft+0xF0); lod=r.u64(plg+0x10); a1=r.u64(lod+0xD8)
    b=r.u64(lod+0xE8)
    print(p.split('/')[-1], 'drawable.bound',hex(r.u64(d+0xF0)),'lod.bound',hex(b),'arch1.bound',hex(r.u64(a1+0x20)),
      'comp flags1',hex(r.u64(b+0x90)),'flags2',hex(r.u64(b+0x98)),'bvh',hex(r.u64(b+0xA8)),'T1',hex(r.u64(b+0x78)),'T2',hex(r.u64(b+0x80)),
      'child0.D1.shadergroup', hex(r.u64(r.u64(r.u64(r.u64(lod+0xD0))+0xA0)+0x10)), 'D1.skel', hex(r.u64(r.u64(r.u64(r.u64(lod+0xD0))+0xA0)+0x18)))
