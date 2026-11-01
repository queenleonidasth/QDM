#!/usr/bin/env node
'use strict';
// Read-only verification of the real post-install profile against the exact
// state checkpoint made by install-slint-verified.ps1. Never starts transfers.
const fs=require('node:fs/promises');
const path=require('node:path');
const assert=require('node:assert/strict');
const {createHash}=require('node:crypto');
const option=(name,fallback)=>{const i=process.argv.indexOf(name);return i<0?fallback:process.argv[i+1];};
const installationPath=path.resolve(option('--installation','docs/evidence/slint-idm-20261027/installed-release.json'));
const reportPath=path.resolve(option('--report','docs/evidence/slint-idm-20261027/upgrade-history-verification.json'));
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const parse=bytes=>JSON.parse(bytes.toString('utf8').replace(/^\uFEFF/,''));
const canonical=value=>Array.isArray(value)?value.map(canonical):value&&typeof value==='object'?Object.fromEntries(Object.keys(value).sort().map(k=>[k,canonical(value[k])])):value;
const recordHash=value=>hash(Buffer.from(JSON.stringify(canonical(value))));
(async()=>{
  const report={schema:1,suite:'Actual installed profile: exact part metadata and worker attribution',startedAt:new Date().toISOString(),status:'running',records:[]};
  try{
    const installation=parse(await fs.readFile(installationPath));
    assert.equal(installation.status,'passed','Installation verification has not passed');
    // Legacy v0.3 initial install and incremental Slint hotfix use distinct backup filenames.
    const beforePath=path.join(installation.backup, installation.installedPath?'state-v2-before.json':'state-v2.json');
    const afterPath=path.join(process.env.APPDATA,'QDM','state-v2.json');
    const beforeBytes=await fs.readFile(beforePath),afterBytes=await fs.readFile(afterPath);
    const before=parse(beforeBytes),after=parse(afterBytes);
    report.installedBinarySha256=hash(await fs.readFile(installation.installedPath || installation.path));
    assert.equal(report.installedBinarySha256,installation.binarySha256);
    report.beforeStateSha256=hash(beforeBytes);report.afterStateSha256=hash(afterBytes);
    assert.equal(after.settings.connections,before.settings.connections,'Connection setting changed');
    report.connections=after.settings.connections;
    for(const old of before.tasks){
      const found=after.tasks.filter(t=>t.view.id===old.view.id);
      assert.equal(found.length,1,`History task missing or duplicated: ${old.view.id}`);
      const current=found[0];
      assert.ok(current.view.downloaded_bytes>=old.view.downloaded_bytes,'Saved byte count decreased');
      const result={id:old.view.id,status:current.view.status,bytes:current.view.downloaded_bytes};
      if(old.view.status==='completed'){
        for(const key of ['status','filename','file_path','downloaded_bytes','total_bytes','sha256','lane_history_estimated']){
          assert.deepEqual(current.view[key],old.view[key],`Completed history field changed: ${key}`);
        }
        const sorted=parts=>[...parts].sort((a,b)=>a.id-b.id);
        assert.deepEqual(sorted(current.parts),sorted(old.parts),'Persisted part metadata changed');
        assert.deepEqual(current.lane_activity,old.lane_activity,'Historical worker attribution changed');
        Object.assign(result,{parts:current.parts.length,workerRecords:current.lane_activity.length,
          partMetadataUnchanged:true,workerAttributionUnchanged:true,
          partMetadataSha256:recordHash(sorted(current.parts)),workerRecordsSha256:recordHash(current.lane_activity)});
        try{await fs.stat(current.view.file_path);result.outputPresent=true;}
        catch(e){if(e.code==='ENOENT')result.outputPresent=false;else throw e;}
      }
      report.records.push(result);
    }
    report.recordsPreserved=before.tasks.length;report.status='passed';
  }catch(error){report.status='failed';report.error=error.stack||String(error);process.exitCode=1;}
  finally{report.finishedAt=new Date().toISOString();await fs.mkdir(path.dirname(reportPath),{recursive:true});await fs.writeFile(reportPath,JSON.stringify(report,null,2)+'\n');}
  console.log(JSON.stringify(report,null,2));
})().catch(error=>{console.error(error.stack||error);process.exitCode=1;});
