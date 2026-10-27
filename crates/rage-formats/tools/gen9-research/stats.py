import struct,sys,os,glob,collections
exec(open('cmp.py').read().split('def shaders')[0])
G9SEM={0:0,4:3,8:14,16:1,20:2,24:4,25:5,28:6,29:7,30:8,31:9,32:10,33:11}
TS={1:4,2:4,3:8,4:0,5:8,6:12,7:16,8:4,9:4,10:4}
def drawables(r,ext):
    if ext=='.ydr': return [0x50000000]
    dp=r.u64(0x50000030); n=r.u16(0x50000038)
    return [r.u64(dp+8*i) for i in range(n)]
def g9shaders(r,d):
    sg=r.u64(d+0x10); sp=r.u64(sg+0x10); n=r.u16(sg+0x18); out=[]
    for i in range(n):
        fx=r.u64(sp+8*i)
        name=r.u32(fx); preset=r.u32(fx+4); pptr=r.u64(fx+8); tptr=r.u64(fx+16); iptr=r.u64(fx+32)
        rb=r.u8(fx+0x39); rbm=r.u32(fx+0x3c)
        nb,nt,nu,ns,npar,u0,u1,mul=r.at(iptr,8)
        bptrs=[r.u64(pptr+8*k) for k in range(nb)] if nb else []
        ps={}; order=[]
        for k in range(npar):
            h,dat=struct.unpack('<II',r.at(iptr+8+8*k,8)); t=dat&3
            if t==0:
                tp=r.u64(tptr+8*((dat>>2)&0xff)); tn=r.cstr(r.u64(tp+0x28)) if tp else None
                ps[h]=('T',tn)
            elif t==3:
                cb=(dat>>2)&0x3f; off=(dat>>8)&0xfff; ln=(dat>>20)&0xfff
                ps[h]=('V',r.at(bptrs[cb]+off,ln))
            else: continue
            order.append(h)
        out.append(dict(name=name,preset=preset,rb=rb,rbm=rbm,ps=ps,order=order))
    return out
def legshaders(r,d):
    sg=r.u64(d+0x10); sp=r.u64(sg+0x10); n=r.u16(sg+0x18); out=[]
    for i in range(n):
        fx=r.u64(sp+8*i)
        pptr=r.u64(fx); name=r.u32(fx+8); cnt=r.u8(fx+0x10); rb=r.u8(fx+0x11); fn=r.u32(fx+0x18); rbm=r.u32(fx+0x20)
        recs=[r.at(pptr+16*k,16) for k in range(cnt)]; vb=sum(16*x[0] for x in recs)
        hs=[r.u32(pptr+16*cnt+vb+4*k) for k in range(cnt)]
        ps=[]
        for h,x in zip(hs,recs):
            dt=x[0]; p=struct.unpack_from('<Q',x,8)[0]
            if dt==0: ps.append((h,0,x[1],r.cstr(r.u64(p+0x28)) if p else None))
            else: ps.append((h,dt,x[1],r.at(p,16*dt)))
        out.append(dict(name=name,file=fn,rb=rb,rbm=rbm,ps=ps,u12=r.u16(fx+0x12)))
    return out
def geoms(r,d,g9):
    res=[]
    for lod in range(4):
        hi=r.u64(d+0x50+8*lod)
        if not hi: continue
        lp=r.u64(hi); lc=r.u16(hi+8)
        for i in range(lc):
            m=r.u64(lp+8*i); gp=r.u64(m+8); gc=r.u16(m+0x10)
            for j in range(gc):
                ge=r.u64(gp+8*j); vb=r.u64(ge+0x18); ib=r.u64(ge+0x38)
                gv=(r.u32(ge+0x58),r.u32(ge+0x5c),r.u16(ge+0x60),r.u16(ge+0x62),r.u16(ge+0x70),r.u16(ge+0x72))
                if g9:
                    cnt=r.u32(vb+8); st=r.u16(vb+12); info=r.u64(vb+0x38)
                    offs=struct.unpack('<52I',r.at(info,208)); sz=r.at(info+208,52); ty=r.at(info+260,52)
                    els=[(k,ty[k],offs[k],sz[k]) for k in range(52) if ty[k]]
                    data=r.at(r.u64(vb+0x18),cnt*st); ixs=r.u16(ib+12)
                    res.append(dict(geom=gv,count=cnt,stride=st,els=els,data=data,ixsize=ixs,ix=r.at(r.u64(ib+0x18),r.u32(ib+8)*ixs),ibbind=r.u32(ib+0x10),vbbind=r.u32(vb+0x10),gvdp=r.u64(ge+0x78)==r.u64(vb+0x18)))
                else:
                    st=r.u16(vb+8); fl=r.u16(vb+10); cnt=r.u32(vb+0x18); info=r.u64(vb+0x30)
                    f,s2,u6,c,t=struct.unpack('<IHBBQ',r.at(info,16))
                    d1=r.u64(vb+0x10); d2=r.u64(vb+0x20)
                    res.append(dict(geom=gv,count=cnt,stride=st,vbflags=fl,flags=f,types=t,data=r.at(d1 or d2,cnt*st),d1eqd2=(d1==d2),ix=r.at(r.u64(ib+0x10),r.u32(ib+8)*2)))
    return res
C=collections.Counter(); ex=collections.defaultdict(list)
presets=collections.Counter(); legorder=collections.Counter()
for base in sys.argv[3:]:
  for f in sorted(glob.glob(os.path.join(sys.argv[1],base,'**/*.yd[rd]'),recursive=True)):
    rel=os.path.relpath(f,sys.argv[1]); lf=os.path.join(sys.argv[2],rel); ext=os.path.splitext(f)[1]
    if not os.path.exists(lf): continue
    g=R(f); l=R(lf); C['files '+ext]+=1
    try:
        gd=drawables(g,ext); ld=drawables(l,ext)
        if len(gd)!=len(ld): C['dict count differs']+=1; continue
        for a,b in zip(gd,ld):
            C['drawables']+=1
            if g.at(a+0x20,0x30)!=l.at(b+0x20,0x30): C['drawable bounds differ']+=1
            if g.at(a+0x70,0x20)!=l.at(b+0x70,0x20): C['drawable lod dist/flags differ']+=1
            for sa,sb in zip(g9shaders(g,a),legshaders(l,b)):
                C['shaders']+=1
                cm=conv.get(sa['name'],{})
                if sa['name']!=sb['name']: C['shader name differs']+=1
                eqfn=sb['file']==joaat(nm(sb['name'])+'.sps')
                if not eqfn: C['legacy FileName != Name.sps']+=1; ex['fn'].append((nm(sb['name']),nm(sb['file']),'0x%08X'%sa['preset']))
                presets[('0x%08X'%sa['preset'], 'eq' if eqfn else 'ne')]+=1
                if sa['preset']==sb['file']: C['preset==legacy FileName hash']+=1
                if sa['rb']!=sb['rb']: C['render bucket differs']+=1
                if sa['rbm']!=sb['rbm']: C['rbm differs']+=1; ex['rbm'].append((hex(sa['rbm']),hex(sb['rbm'])))
                if sb['u12']!=32768: C['legacy unknown_12h != 32768']+=1
                if sa['name'] not in conv: C['shader missing from XML']+=1; ex['nox'].append(nm(sa['name']))
                g2l={}
                for h in sa['order']:
                    t,old=cm.get(h,(None,None)); lh=joaat(old) if old else h; g2l[lh]=h
                lhs=[p[0] for p in sb['ps']]
                C['legacy params']+=len(lhs); C['gen9 tex+cb params']+=len(sa['order'])
                for (lh,dt,u1,val) in sb['ps']:
                    if lh not in g2l: C['legacy param w/o gen9 source']+=1; ex['nosrc'].append((nm(sb['name']),nm(lh))); continue
                    k,gv=sa['ps'][g2l[lh]]
                    if dt==0:
                        if k!='T': C['type mismatch']+=1
                        elif (gv or '').lower()!=(val or '').lower(): C['texture name differs']+=1; ex['tex'].append((nm(sb['name']),nm(lh),gv,val))
                        else: C['texture equal']+=1
                    else:
                        if k!='V': C['type mismatch']+=1; continue
                        n=len(gv); lv=val[:n]
                        if dt!=max(1,n//16): C['array size differs']+=1; ex['arr'].append((nm(sb['name']),nm(lh),dt,n))
                        if gv==lv: C['vector equal (gen9 length prefix)']+=1
                        else: C['vector differs']+=1; ex['vec'].append((nm(sb['name']),nm(lh),struct.unpack('<%df'%(n//4),gv),struct.unpack('<%df'%(min(len(val),64)//4),val[:64])))
                        if n<16 and any(val[n:16]): C['legacy pad lanes nonzero (gen9 shorter)']+=1; ex['pad'].append((nm(sb['name']),nm(lh),n,struct.unpack('<4f',val[:16])))
                C['gen9 mapped params absent from legacy']+=len(set(g2l)-set(lhs))
                kinds=''.join('T' if p[1]==0 else 'V' for p in sb['ps']); legorder['T*V*' if kinds==''.join(sorted(kinds)) else 'mixed']+=1
                mp=[p for p in sb['ps'] if p[0] in g2l]
                tpos=[sa['order'].index(g2l[p[0]]) for p in mp if p[1]==0]; vpos=[sa['order'].index(g2l[p[0]]) for p in mp if p[1]!=0]
                C['legacy vec order == gen9 order' if vpos==sorted(vpos) else 'legacy vec order != gen9 order']+=1
                C['legacy tex order == gen9 order' if tpos==sorted(tpos) else 'legacy tex order != gen9 order']+=1
                ub=[p[2] for p in sb['ps'] if p[1]==0]; exp=[i+2 for i,p in enumerate(sb['ps']) if p[1]==0]
                C['tex unknown_1h == i+2' if ub==exp else 'tex unknown_1h other']+=1
                if ub!=exp: ex['u1h'].append((nm(sb['name']),ub,exp))
            for va,vl in zip(geoms(g,a,True),geoms(l,b,False)):
                C['geoms']+=1
                if va['geom']!=vl['geom']: C['geom fields differ']+=1; ex['geom'].append((va['geom'],vl['geom']))
                if va['stride']!=vl['stride']: C['stride differs']+=1
                if vl['vbflags']: C['legacy vb flags nonzero']+=1
                if not vl['d1eqd2']: C['legacy d1!=d2']+=1
                if va['ixsize']!=2: C['gen9 32-bit indices']+=1
                if va['ix']!=vl['ix']: C['indices differ']+=1
                if not va['gvdp']: C['gen9 geom vertexdata ptr != vb data']+=1
                C['bind ib=%x vb=%x'%(va['ibbind'],va['vbbind'])]+=1
                C['legacy types %x'%vl['types']]+=1
                flags=0; ok=True
                for (k,t,o,s) in va['els']:
                    if k in G9SEM: flags|=1<<G9SEM[k]
                    else: ok=False; ex['sem'].append(k)
                if flags!=vl['flags']: C['decl flags differ']+=1; ex['flags'].append((flags,vl['flags'],hex(vl['types']),va['els']))
                if not ok: C['gen9 semantic w/o legacy']+=1
                T=vl['types']
                def loff(bit): return sum(TS.get((T>>(4*q))&0xf,0) for q in range(bit) if vl['flags']>>q&1)
                same=True
                for (k,t,o,s) in va['els']:
                    if k not in G9SEM: continue
                    bit=G9SEM[k]; sz=TS.get((T>>(4*bit))&0xf,0)
                    if not (vl['flags']>>bit&1): continue
                    for v in range(va['count']):
                        if va['data'][o+v*va['stride']:o+v*va['stride']+sz]!=vl['data'][loff(bit)+v*vl['stride']:loff(bit)+v*vl['stride']+sz]: same=False;break
                    if not same: ex['bytes'].append((k,t,bit,hex(T))); break
                C['vertex bytes equal after remap' if same else 'vertex bytes DIFFER after remap']+=1
                C['g9 elems '+' '.join('%d:%d'%(k,t) for k,t,o,s in va['els'])]+=1
    except Exception as e:
        C['ERR']+=1; ex['err'].append((rel,repr(e)))
for k,v in sorted(C.items()):
    if v: print(v,k)
print('presets',presets.most_common(12)); print('legorder',legorder)
for k,v in ex.items(): print(k,len(v),collections.Counter(map(str,v)).most_common(10))
