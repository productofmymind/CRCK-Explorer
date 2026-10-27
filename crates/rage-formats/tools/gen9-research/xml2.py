import xml.etree.ElementTree as ET, collections
r=ET.parse(r'C:\dev\gta\v\tools\reference\source\the reference library\ShadersGen9Conversion.xml').getroot()
coll=[];nonhash_noold=collections.Counter(); texidx_gap=[]; buf_overrun=[]; nomappedparams=[]
for it in r.findall('Item'):
    n=it.findtext('Name'); ps=list(it.find('Parameters'))
    olds={p.get('old').lower() for p in ps if p.get('old')}
    for p in ps:
        if p.get('old') is None and p.get('type') in('Texture','CBuffer'):
            if p.get('name').lower() in olds: coll.append((n,p.get('name')))
            if not p.get('name').lower().startswith('hash_'): nonhash_noold[(p.get('type'),p.get('name').lower())]+=1
    tix=sorted(int(p.get('index')) for p in ps if p.get('type')=='Texture')
    if tix!=list(range(len(tix))): texidx_gap.append(n)
    bs=[int(x) for x in (it.findtext('BufferSizes') or '').split()]
    for p in ps:
        if p.get('type')=='CBuffer':
            b=int(p.get('buffer')); o=int(p.get('offset')); l=int(p.get('length'))
            if b>=len(bs) or o+l>bs[b]: buf_overrun.append((n,p.get('name')))
    leg=[p for p in ps if p.get('old') and p.get('type') in ('Texture','CBuffer')]
    if not leg: nomappedparams.append(n)
print('no-old gen9 name colliding with another param old in same shader:',coll)
print('non-hash no-old tex/cbuf names:',nonhash_noold.most_common())
print('texture index gaps',texidx_gap[:10],len(texidx_gap))
print('cbuffer overrun',buf_overrun[:10],len(buf_overrun))
print('shaders with zero legacy-mapped params',nomappedparams)
