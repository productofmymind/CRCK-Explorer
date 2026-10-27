import zlib,struct,sys,os,glob,collections,xml.etree.ElementTree as ET
def joaat(s):
    h=0
    for c in s.lower().encode():
        h=(h+c)&0xffffffff; h=(h+(h<<10))&0xffffffff; h^=h>>6
    h=(h+(h<<3))&0xffffffff; h^=h>>11; h=(h+(h<<15))&0xffffffff; return h
names={}
for l in open('C:/dev/gta/v/tools/rage-formats/src/names/reference.txt',encoding='utf8',errors='ignore'):
    l=l.strip()
    if l: names[joaat(l)]=l
for f in [r'C:\dev\gta\v\tools\rage-formats\src\names\shader_params.txt',r'C:\dev\gta\v\tools\rage-formats\src\names\core.txt']:
    for l in open(f,encoding='utf8',errors='ignore'):
        l=l.strip()
        if l and not l.startswith('#'): names[joaat(l)]=l
X=ET.parse(r'C:\dev\gta\v\tools\reference\source\the reference library\ShadersGen9Conversion.xml').getroot()
conv={}
for it in X.findall('Item'):
    n=it.findtext('Name').lower(); names[joaat(n)]=n; names[joaat(n+'.sps')]=n+'.sps'
    m={}
    for p in it.find('Parameters'):
        nm=p.get('name')
        h=int(nm[5:],16) if nm.lower().startswith('hash_') else joaat(nm)
        if not nm.lower().startswith('hash_'): names[h]=nm.lower()
        if p.get('old'): names[joaat(p.get('old'))]=p.get('old').lower()
        m[h]=(p.get('type'),p.get('old'))
    conv[joaat(n)]=m
def size(f):
    s=[((f>>27)&1),((f>>26)&1)<<1,((f>>25)&1)<<2,((f>>24)&1)<<3,((f>>17)&0x7f)<<4,((f>>11)&0x3f)<<5,((f>>7)&0xf)<<6,((f>>5)&3)<<7,((f>>4)&1)<<8]
    return (0x200<<(f&0xf))*sum(s)
class R:
    def __init__(s,path):
        d=open(path,'rb').read(); s.ver=d[4]; sf,gf=struct.unpack_from('<II',d,8)
        s.ver=((sf>>28)<<4)|(gf>>28)
        b=zlib.decompress(d[16:],-15); ss=size(sf); s.sys=b[:ss]; s.gfx=b[ss:]
    def at(s,va,n):
        if va>>28==5: o=va-0x50000000; return s.sys[o:o+n]
        if va>>28==6: o=va-0x60000000; return s.gfx[o:o+n]
        raise Exception(hex(va))
    def u8(s,va): return s.at(va,1)[0]
    def u16(s,va): return struct.unpack('<H',s.at(va,2))[0]
    def u32(s,va): return struct.unpack('<I',s.at(va,4))[0]
    def u64(s,va): return struct.unpack('<Q',s.at(va,8))[0]
    def cstr(s,va):
        b=s.at(va,256); return b[:b.index(0)].decode()
def nm(h): return names.get(h,'0x%08X'%h)
def shaders(r,g9):
    root=0x50000000; sg=r.u64(root+0x10)
    sp=r.u64(sg+0x10); n=r.u16(sg+0x18); out=[]
    for i in range(n):
        fx=r.u64(sp+8*i)
        if g9:
            name=r.u32(fx); preset=r.u32(fx+4); pptr=r.u64(fx+8); tptr=r.u64(fx+16); iptr=r.u64(fx+32)
            rb=r.u8(fx+0x39); pds=r.u16(fx+0x3a); rbm=r.u32(fx+0x3c)
            nb,nt,nu,ns,npar,u0,u1,u2=r.at(iptr,8)
            ps=[]
            for k in range(npar):
                h,dat=struct.unpack('<II',r.at(iptr+8+8*k,8)); t=dat&3
                ps.append((nm(h),['Tex','Unk','Smp','CB'][t], (dat>>2)&0x3f if t==3 else (dat>>2)&0xff,(dat>>8)&0xfff,(dat>>20)&0xfff))
            out.append(dict(name=nm(name),preset='0x%08X'%preset+'('+nm(preset)+')',rb=rb,rbm=hex(rbm),params=ps,multi=u2))
        else:
            pptr=r.u64(fx); name=r.u32(fx+8); cnt=r.u8(fx+0x10); rb=r.u8(fx+0x11); u12=r.u16(fx+0x12); psz=r.u16(fx+0x14); pds=r.u16(fx+0x16); fn=r.u32(fx+0x18); rbm=r.u32(fx+0x20); tc=r.u8(fx+0x27)
            recs=[r.at(pptr+16*k,16) for k in range(cnt)]; vb=sum(16*x[0] for x in recs)
            hs=[r.u32(pptr+16*cnt+vb+4*k) for k in range(cnt)]
            out.append(dict(name=nm(name),file=nm(fn),rb=rb,rbm=hex(rbm),u12=u12,psz=psz,pds=pds,tc=tc,params=[(nm(h),x[0],x[1]) for h,x in zip(hs,recs)]))
    return out
def geoms(r,g9):
    root=0x50000000; hi=r.u64(root+0x50); res=[]
    if not hi: return res
    lp=r.u64(hi); lc=r.u16(hi+8)
    for i in range(lc):
        m=r.u64(lp+8*i); gp=r.u64(m+8); gc=r.u16(m+0x10)
        for j in range(gc):
            ge=r.u64(gp+8*j); vb=r.u64(ge+0x18); ib=r.u64(ge+0x38)
            gv=(r.u32(ge+0x58),r.u32(ge+0x5c),r.u16(ge+0x60),r.u16(ge+0x62),r.u16(ge+0x70))
            if g9:
                cnt=r.u32(vb+8); st=r.u16(vb+12); bf=r.u32(vb+0x10); info=r.u64(vb+0x38)
                offs=struct.unpack('<52I',r.at(info,208)); sz=r.at(info+208,52); ty=r.at(info+260,52); data=r.u64(info+312)
                els=[(k,ty[k],offs[k],sz[k]) for k in range(52) if ty[k]]
                res.append(dict(geom=gv,count=cnt,stride=st,bind=hex(bf),els=els,vsize=(data>>2)&0xff,ib=(r.u32(ib+8),r.u16(ib+12),hex(r.u32(ib+0x10)))))
            else:
                st=r.u16(vb+8); fl=r.u16(vb+10); cnt=r.u32(vb+0x18); info=r.u64(vb+0x30)
                f,s2,u6,c,t=struct.unpack('<IHBBQ',r.at(info,16))
                res.append(dict(geom=gv,count=cnt,stride=st,vbflags=fl,declflags=f,declstride=s2,types=hex(t),d1=hex(r.u64(vb+0x10)),d2=hex(r.u64(vb+0x20)),ib=r.u32(ib+8)))
    return res
import json
files=sorted(glob.glob(sys.argv[1]+'/**/*.ydr',recursive=True))
pairs=collections.Counter()
for f in files:
    rel=os.path.relpath(f,sys.argv[1]); lf=os.path.join(sys.argv[2],rel)
    if not os.path.exists(lf): continue
    g=R(f); l=R(lf)
    print('==',rel,'ver',g.ver,l.ver)
    try:
        gs=shaders(g,True); ls=shaders(l,False)
        for a,b in zip(gs,ls):
            print(' G9 ',a['name'],a['preset'],'rb',a['rb'],a['rbm'],'multi',a['multi']); print('    ',a['params'])
            print(' LEG',b['name'],b['file'],'rb',b['rb'],b['rbm'],'u12',b['u12'],'psz',b['psz'],'pds',b['pds'],'tc',b['tc']); print('    ',b['params'])
            pairs[(a['preset'],b['file'])]+=1
        for a,b in zip(geoms(g,True),geoms(l,False)): print(' VG9',a); print(' VLG',b)
    except Exception as e: print(' ERR',e)
print(pairs)
