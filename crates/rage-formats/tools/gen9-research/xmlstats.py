import xml.etree.ElementTree as ET, collections
r=ET.parse(r'C:\dev\gta\v\tools\reference\source\the reference library\ShadersGen9Conversion.xml').getroot()
items=r.findall('Item')
print('shaders',len(items))
types=collections.Counter(); noold=collections.Counter(); noold_names=collections.Counter()
total=0; dup_old=[]; dup_name=[]; fn_mismatch=[]; same=0; names=set(); dupshader=[]
oldeqname=0; nobuf=0; bufcounts=collections.Counter(); hashnames=0; lens=collections.Counter()
for it in items:
    n=it.findtext('Name'); fn=it.findtext('FileName')
    if n in names: dupshader.append(n)
    names.add(n)
    if fn!=n+'.sps': fn_mismatch.append((n,fn))
    bs=(it.findtext('BufferSizes') or '').split(); bufcounts[len(bs)]+=1
    olds=collections.Counter(); nm=collections.Counter()
    for p in it.find('Parameters'):
        t=p.get('type'); types[t]+=1; total+=1
        name=p.get('name'); old=p.get('old')
        if name.lower().startswith('hash_'): hashnames+=1
        nm[name.lower()]+=1
        if old is None:
            noold[t]+=1; noold_names[(t,name.lower())]+=1
        else:
            olds[old.lower()]+=1
            if old.lower()==name.lower(): oldeqname+=1
        if t=='CBuffer': lens[int(p.get('length'))]+=1
    for k,v in olds.items():
        if v>1: dup_old.append((n,k,v))
    for k,v in nm.items():
        if v>1: dup_name.append((n,k,v))
print('param types',dict(types),'total',total)
print('params without old by type',dict(noold))
print('old==name',oldeqname,'hash_ names',hashnames)
print('buffer count distribution',dict(bufcounts))
print('dup shader names',dupshader)
print('FileName != Name.sps',len(fn_mismatch),fn_mismatch[:15])
print('shaders with duplicated old (ambiguous g9->legacy many-to-one)',len(dup_old),dup_old[:20])
print('shaders with dup gen9 name',dup_name[:20])
print('top no-old names',noold_names.most_common(40))
print('cbuffer lengths',sorted(lens.items()))
# cbuffer params without old
cb=[(it.findtext('Name'),p.get('name')) for it in items for p in it.find('Parameters') if p.get('type')=='CBuffer' and p.get('old') is None]
print('cbuffer-no-old count',len(cb), cb[:40])
tx=[(it.findtext('Name'),p.get('name')) for it in items for p in it.find('Parameters') if p.get('type')=='Texture' and p.get('old') is None]
print('texture-no-old distinct', collections.Counter(n.lower() for _,n in tx).most_common())
un=[(it.findtext('Name'),p.get('name'),p.attrib) for it in items for p in it.find('Parameters') if p.get('type')=='Unknown']
print('Unknown params',len(un),un[:10])
