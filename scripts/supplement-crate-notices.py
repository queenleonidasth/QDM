#!/usr/bin/env python3
"""Recover upstream notice files omitted from published crate archives.

Only reads GitHub at each crate archive's exact .cargo_vcs_info.json commit.
No downloaded code is executed. Evidence records every original URL and hash.
"""
import argparse, concurrent.futures, hashlib, json, re, tomllib
from pathlib import Path
from urllib.parse import urlsplit
from urllib.request import Request, urlopen

p=argparse.ArgumentParser();p.add_argument('--cargo-home',type=Path,required=True)
p.add_argument('--root',type=Path,default=Path(__file__).resolve().parents[1]);a=p.parse_args()
root=a.root.resolve();report_path=root/'docs/third-party/dependency-licenses-0.3.0.json'
report=json.loads(report_path.read_text(encoding='utf-8'));registries=list((a.cargo_home/'registry/src').glob('*'))
pending=[x for x in report['unavailable_or_missing'] if x['reason']=='crate has no discovered license text']
jobs={};failures=[];recovered=[]
for item in pending:
    src=next((r/f"{item['name']}-{item['version']}" for r in registries if (r/f"{item['name']}-{item['version']}").is_dir()),None)
    try:
        meta=tomllib.loads((src/'Cargo.toml').read_text(encoding='utf-8'))['package']
        repo=urlsplit(meta['repository'].replace('git+','').replace('git://','https://'))
        if repo.hostname!='github.com':raise ValueError('Non-GitHub upstream requires manual notice collection')
        owner,name=repo.path.strip('/').removesuffix('.git').split('/')[:2]
        commit=json.loads((src/'.cargo_vcs_info.json').read_text(encoding='utf-8'))['git']['sha1']
        if not re.fullmatch(r'[0-9a-f]{40}',commit):raise ValueError('Missing exact commit')
        jobs.setdefault((owner,name,commit),[]).append(item)
    except Exception as e:failures.append({**item,'detail':str(e)})

def fetch(url):
    req=Request(url,headers={'User-Agent':'QDM-License-Notice-Collector/0.3.0'})
    with urlopen(req,timeout=15) as response:
        data=response.read(2*1024*1024+1)
        if len(data)>2*1024*1024:raise ValueError('Notice exceeds size limit')
        return data
def collect(job):
    (owner,name,commit),items=job;found=[];errors=[]
    base=f'https://raw.githubusercontent.com/{owner}/{name}/{commit}/'
    for filename in ['LICENSE','LICENSE-MIT','LICENSE-APACHE','LICENSE.md','LICENSE.txt','COPYING']:
        try:found.append((filename,base+filename,fetch(base+filename)))
        except Exception as e:errors.append(str(e))
    return items,found,errors

with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
    for items,found,errors in pool.map(collect,jobs.items()):
        for item in items:
            if not found:failures.append({**item,'detail':'; '.join(errors)});continue
            evidence=[]
            for filename,url,data in found:
                dest=root/'docs/third-party/crates'/f"{item['name']}-{item['version']}"/('UPSTREAM-'+filename)
                dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(data)
                evidence.append({'path':str(dest.relative_to(root)).replace('\\','/'),'source_url':url,'sha256':hashlib.sha256(data).hexdigest()})
            recovered.append({**item,'files':evidence})
            next(x for x in report['crates'] if x['name']==item['name'] and x['version']==item['version'])['files'].extend(evidence)
            report['unavailable_or_missing']=[x for x in report['unavailable_or_missing'] if not(x['name']==item['name'] and x['version']==item['version'])]
report['supplemental_notice_provenance']={'method':'Exact upstream Git commit recorded in published crate archive; notice files only','recovered':recovered,'failures':failures}
report_path.write_text(json.dumps(report,indent=2)+'\n',encoding='utf-8')
print(json.dumps({'recovered_crates':len(recovered),'missing':failures}))
