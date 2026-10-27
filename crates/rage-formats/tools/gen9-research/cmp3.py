import sys, struct
sys.path.insert(0,'.')
from fragwalk import walk
a=walk(sys.argv[1]); b=walk(sys.argv[2])
for k in sys.argv[3:]:
    x=a[2][k][2]; y=b[2][k][2]; n=0
    for o in range(0,len(x),4):
        if x[o:o+4]!=y[o:o+4]:
            n+=1
            if n<=8: print(k, hex(o), x[o:o+4].hex(), y[o:o+4].hex(), struct.unpack('<f',x[o:o+4])[0], struct.unpack('<f',y[o:o+4])[0])
    print(k,'dwords differing',n,'of',len(x)//4)
r=a[0]; d=r.u64(0x50000000+0x30); lod=r.u64(d+0x50); print('legacy high lod ptr',hex(lod))
