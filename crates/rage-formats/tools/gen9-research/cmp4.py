import sys, struct
sys.path.insert(0,'.')
from fragwalk import R, mask
def bounds(p):
    r=R(p); ft=0x50000000; plg=r.u64(ft+0xF0); lod=r.u64(plg+0x10); b=r.u64(lod+0xE8)
    out={'comp':mask(r.sl(b,0xB0))}
    n=r.u16(b+0xA0); cp=r.u64(b+0x70)
    for i in range(n):
        c=r.u64(cp+8*i)
        if not c: continue
        t=r.u8(c+0x10)
        hdr=mask(r.sl(c,0x150 if t==8 else 0x130 if t==4 else 0x80))
        out['child%d(t%d)'%(i,t)]=hdr
        if t in (4,8):
            vp=r.u64(c+0xC8) if False else None
    for nm,off,sz in (('T1',0x78,64),('T2',0x80,64),('BB',0x88,32),('F1',0x90,8),('F2',0x98,8)):
        v=r.u64(b+off)
        if v: out[nm]=r.sl(v,sz*n)
    return out
a=bounds(sys.argv[1]); b=bounds(sys.argv[2])
same=0
for k in a:
    if a[k]==b[k]: same+=1
    else:
        x,y=a[k],b[k]; d=[hex(o) for o in range(0,len(x),4) if x[o:o+4]!=y[o:o+4]]
        print(k,len(x),'diff dwords',d[:10])
print('same',same,'of',len(a))
