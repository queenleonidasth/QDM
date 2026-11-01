'use strict';
const fs=require('node:fs/promises');
const os=require('node:os');
const path=require('node:path');
const {CliClient,fileHash}=require('./helpers/cli-client.cjs');
const {createHttpFixture,MiB}=require('./helpers/http-fixture.cjs');
const bin=path.resolve(process.argv[2]||'target/release/QDM.exe');
(async()=>{
  const root=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-trace-perf-'));
  const origin=await createHttpFixture({sizeBytes:128*MiB+137,chunkSize:64*1024,delayMs:0});
  await fs.mkdir(path.join(root,'files'),{recursive:true});
  const cli=new CliClient(bin,path.join(root,'state'),path.join(root,'files'));
  const lines=[];
  cli.child.stderr.on('data',chunk=>{
    for(const line of chunk.toString('utf8').split(/\r?\n/)) {
      if(line.startsWith('QDM_TRANSFER_TRACE ')){
        try{lines.push(JSON.parse(line.slice(19)));}catch{}
      }
    }
  });
  try{
    await cli.settings({connections:32,max_concurrent:1});
    const begun=Date.now();
    const id=await cli.add({url:origin.url,filename:'sample.bin',expected_sha256:origin.hash()});
    const final=await cli.terminal(id,100000);
    const end=Date.now();
    if(final.status!=='completed')throw new Error(JSON.stringify(final));
    if(await fileHash(final.file_path)!==origin.hash())throw new Error('SHA check failed');
    const events={};
    for(const t of lines){
      const group=events[t.event]||(events[t.event]={count:0,first:t.atMs,last:t.atMs,totalDurationMs:0,maxDurationMs:0});
      group.count++;group.last=t.atMs;
      group.totalDurationMs+=(t.elapsedMs||0);
      group.maxDurationMs=Math.max(group.maxDurationMs,t.elapsedMs||0);
    }
    console.log(JSON.stringify({binary:bin,elapsedMs:end-begun,traceEvents:lines.length,
      eventGroups:events,
      firstBody:lines.filter(x=>x.event==='first-body').slice(0,2),
      finalEvent:lines.filter(x=>x.event==='direct-published'||x.event==='published').slice(-1)},null,2));
  }finally{
    await cli.shutdown().catch(()=>cli.crash());
    await origin.close();
    await fs.rm(root,{recursive:true,force:true,maxRetries:10,retryDelay:100});
  }
})().catch(e=>{console.error(e.stack||e);process.exitCode=1;});
