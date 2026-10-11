#!/usr/bin/env python3
"""Append resolved dependency license declarations without private machine paths."""
import json, pathlib, subprocess
root=pathlib.Path(__file__).resolve().parent.parent
cargo=json.loads(subprocess.check_output([str(pathlib.Path.home()/'.cargo/bin/cargo'),'metadata','--format-version','1','--locked'],cwd=root,text=True))
rust=sorted({(p['name'],p['version'],p.get('license') or 'UNDECLARED — REVIEW',p.get('repository') or '') for p in cargo['packages']})
node=set()
for path in (root/'node_modules').rglob('package.json'):
 try:
  p=json.loads(path.read_text()); name=p.get('name'); version=p.get('version')
  if not name or not version:continue
  license=p.get('license') or p.get('licenses') or 'UNDECLARED — REVIEW'
  if not isinstance(license,str):license=json.dumps(license)
  node.add((name,version,license))
 except (ValueError,OSError):pass
notice=root/'THIRD_PARTY_NOTICES.md'
base=notice.read_text().split('<!-- generated inventory -->')[0]
out=['<!-- generated inventory -->','\n## Resolved Rust dependencies\n','| Package | Version | Declared license |','|---|---|---|']
out += [f'| {a} | {b} | {c.replace("|", "or")} |' for a,b,c,_ in rust]
out += ['\n## Resolved npm dependencies (including build and test tools)\n','| Package | Version | Declared license |','|---|---|---|']
out += [f'| {a} | {b} | {c.replace("|", "or")} |' for a,b,c in sorted(node)]
notice.write_text(base+'\n'+'\n'.join(out)+'\n')
print(f'Inventoried {len(rust)} Rust and {len(node)} npm packages; license declarations require binary-distribution review.')
