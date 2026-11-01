'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs/promises');
const path=require('node:path');
const os=require('node:os');
const crypto=require('node:crypto');
const {execFileSync}=require('node:child_process');
const {CliClient}=require('./helpers/cli-client.cjs');
const {openState,journalMode}=require('./helpers/sqlite-state.cjs');
const arg=(k,f)=>{const i=process.argv.indexOf(k);return i<0?f:process.argv[i+1];};
(async()=>{
  const binary=path.resolve(arg('--binary','target/release/QDM.exe'));
  const source=path.join(process.env.APPDATA,'QDM','state-v2.json');
  const original=await fs.readFile(source);
  const expected=crypto.createHash('sha256').update(original).digest('hex');
  const parsed=JSON.parse(original);
  const expectedIds=parsed.tasks.filter(t=>t.view.status==='completed').map(t=>t.view.id);
  const root=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-import-v2-'));
  const data=path.join(root,'state'), download=path.join(root,'files');
  await fs.mkdir(data,{recursive:true});await fs.mkdir(download,{recursive:true});
  await fs.writeFile(path.join(data,'state-v2.json'),original);
  let client;
  try {
    client=new CliClient(binary,data,download);
    const snapshot=await client.snapshot();
    assert.equal(snapshot.downloads.length,expectedIds.length,'Only completed legacy jobs should migrate');
    for(const id of expectedIds)assert.ok(snapshot.downloads.some(t=>t.id===id));
    assert.equal(snapshot.settings.connections,parsed.settings.connections);
    await client.shutdown();client=null;
    const saved=openState(data);
    assert.equal(saved.tasks.length,expectedIds.length);
    assert.ok(saved.tasks.every(t=>t.parts.length===0 && t.view.status==='completed'));
    assert.equal(journalMode(data),'wal');
    const inspect=JSON.parse(execFileSync('python',[path.join(__dirname,'../scripts/check_sqlite_install.py'), path.join(data,'state-v3.sqlite3')],{encoding:'utf8'}));
    assert.equal(inspect.downloads,expectedIds.length);
    assert.equal(inspect.integrity,'ok');
    assert.equal(inspect.journal,'wal');
    const now=await fs.readFile(source);
    assert.equal(crypto.createHash('sha256').update(now).digest('hex'),expected,'Original v2 was modified');
    console.log(JSON.stringify({status:'PASS',importedCompletedJobs:saved.tasks.length,
      originalStateUnchanged:true,version:3,wal:true,sourceSha256:expected},null,2));
  }finally{
    if(client)await client.shutdown().catch(()=>client.crash());
    await fs.rm(root,{recursive:true,force:true,maxRetries:10,retryDelay:100});
  }
})().catch(e=>{console.error(e.stack||e);process.exitCode=1;});
