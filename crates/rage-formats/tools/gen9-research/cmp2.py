import sys, struct
sys.path.insert(0,'.')
from fragwalk import walk
a=walk(sys.argv[1]); b=walk(sys.argv[2])
seen=set()
for k in a[2]:
    if k not in b[2]: continue
    base=k.split('[')[0] + (k.split(']')[1] if ']' in k else '')
    x=a[2][k][2]; y=b[2][k][2]
    va=struct.unpack_from('<I',x)[0]; vb=struct.unpack_from('<I',y)[0]
    if base not in seen:
        seen.add(base); print('%-22s vft legacy=%d (0x%x) gen9=%d (0x%x)'%(base,va,va,vb,vb))
for k in ['FragDrawable','FragType','BoneTransforms','LOD1','LOD1.Arch1']:
    x=a[2][k][2]; y=b[2][k][2]
    for o in range(0,len(x),4):
        if x[o:o+4]!=y[o:o+4]: print(k, hex(o), x[o:o+4].hex(), y[o:o+4].hex(), struct.unpack('<f',x[o:o+4])[0] if b'P' not in x[o:o+4] else '', struct.unpack('<f',y[o:o+4])[0] if b'P' not in y[o:o+4] else '')
