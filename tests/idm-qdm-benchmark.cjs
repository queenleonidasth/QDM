#!/usr/bin/env node
'use strict';
// Actual installed IDM vs the actual Slint QDM GUI. No registry writes, queue
// start (/s), global IDM shutdown (/q), or user download/history deletions.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const syncFs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const readline = require('node:readline');
const { spawn } = require('node:child_process');
const { Readable, Transform } = require('node:stream');
const { pipeline } = require('node:stream/promises');
const { CliClient, delay, waitFor, fileHash } = require('./helpers/cli-client.cjs');
const { createBenchmarkOrigin } = require('./helpers/benchmark-origin.cjs');
const getArg=(name,fallback)=>{const i=process.argv.indexOf(name);return i<0?fallback:process.argv[i+1];};
const binary=path.resolve(getArg('--binary','target/release/QDM.exe'));
const idm=path.resolve(getArg('--idm',path.join(process.env['ProgramFiles(x86)']||'C:\\Program Files (x86)','Internet Download Manager','IDMan.exe')));
const idmPid=Number(getArg('--idm-pid','0'));
const smoke=process.argv.includes('--smoke');
const requestedScenario=getArg('--scenario',smoke?'fast':'all');
const reportPath=path.resolve(getArg('--report','docs/evidence/slint-idm-20261027/idm-qdm-benchmark.json'));
const workParent=path.resolve(getArg('--workdir',os.tmpdir()));
const bytes=Number(getArg('--bytes',String((smoke?8:256)*1024*1024+137)));
const timeoutMs=Number(getArg('--timeout-ms','240000'));
const connections=Number(getArg('--connections','32'));
const betweenRunsMs=Number(getArg('--between-runs-ms','750'));
const order=getArg('--order',smoke?'IDM,QDM':'IDM,QDM,QDM,IDM').split(',').map(v=>v.trim().toUpperCase());
delete process.env.QDM_TEST_ASSEMBLY_DELAY_MS;
const publicUrls={
  'public-sin':'https://sin1-sgp.speedtest.network.ovh.net/files/100Mb.dat',
  'public-bom':'https://bom1-ynm1.speedtest.network.ovh.net/files/100Mb.dat',
  'public-alpine':'https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-virt-3.24.3-x86_64.iso',
};
const publicPinnedHashes={
  'public-alpine':{
    sha256:'cdc1bc0efc64d8d611bf3733f54868172a545888dfd726f518bf279bcb0ff1af',
    downloadPage:'https://alpinelinux.org/downloads/',
  },
};

function processExit(child) { return new Promise((resolve,reject)=>{child.once('error',reject);child.once('exit',(code,signal)=>resolve({code,signal}));}); }
async function powershell(command) {
  const child=spawn('powershell.exe',['-NoProfile','-NonInteractive','-Command',command],{windowsHide:true,stdio:['ignore','pipe','pipe']});
  let stdout='',stderr='';child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);
  const exit=await processExit(child);if(exit.code!==0)throw new Error(`PowerShell failed: ${stderr}`);return stdout.trim();
}
async function idmPreflight(report,workdir,index){
  const configPath=path.join(workdir,`idm-preflight-${index}.config.json`);
  const outputPath=path.join(workdir,`idm-preflight-${index}.json`);
  await fs.writeFile(configPath,JSON.stringify({processId:idmPid,binaryPath:idm,
    binarySha256:report.idmBinarySha256,processStartTime:report.idmProcess.startTime,
    workdir,reportPath:outputPath,passedRows:report.runs.filter(row=>row.application==='IDM'&&row.status==='passed')
      .map(({application,status,file,actualSha256,expectedSha256})=>({application,status,file,actualSha256,expectedSha256}))}));
  const child=spawn('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',
    path.join(__dirname,'helpers','idm-preflight.ps1'),'-ConfigPath',configPath],{windowsHide:true,stdio:['ignore','pipe','pipe']});
  let stderr='';child.stdout.resume();child.stderr.on('data',bytes=>stderr=(stderr+bytes).slice(-4000));
  let timedOut=false;
  const timer=setTimeout(()=>{timedOut=true;child.kill();},20000);
  let exit;
  try{exit=await processExit(child);}finally{clearTimeout(timer);}
  let result;
  try{result=JSON.parse((await fs.readFile(outputPath,'utf8')).replace(/^\uFEFF/,''));}
  catch{result={status:'failed',error:timedOut?'IDM preflight helper exceeded20s; only the helper process was stopped':`IDM preflight produced no valid report: ${stderr}`};}
  if(exit.code!==0&&result.status==='passed'){result.status='failed';result.error='IDM preflight helper exited unsuccessfully';}
  return {...result,reportPath:outputPath};
}
async function monitor(processId,file,expectedBytes,workdir,index) {
  const configPath=path.join(workdir,`monitor-${index}.json`);
  await fs.writeFile(configPath,JSON.stringify({processId,file,expectedBytes,pollMs:50,timeoutMs}));
  const child=spawn('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'helpers','file-process-monitor.ps1'),'-ConfigPath',configPath],{windowsHide:true,stdio:['ignore','pipe','pipe']});
  let readyResolve,readyReject,doneResolve,doneReject;
  const ready=new Promise((resolve,reject)=>{readyResolve=resolve;readyReject=reject;});
  const done=new Promise((resolve,reject)=>{doneResolve=resolve;doneReject=reject;});
  // The caller waits for ready first; prevent an early monitor error becoming
  // an unhandled rejection while that first promise is still pending.
  done.catch(()=>{});
  const samples=[];let completed=null,baseline=null,stderr='';
  child.stderr.on('data',b=>stderr+=b);
  const lines=readline.createInterface({input:child.stdout});
  lines.on('line',line=>{let value;try{value=JSON.parse(line);}catch{return;}
    if(value.type==='ready'){baseline=value;readyResolve(value);}
    else if(value.type==='sample')samples.push(value);
    else if(value.type==='complete'){completed=value;}
    else if(value.type==='error'){const error=new Error(value.message);readyReject(error);doneReject(error);}
  });
  const exited=processExit(child).then(exit=>{
    if(exit.code===0&&completed){doneResolve({baseline,samples,completed});}
    else {const error=new Error(`File/process monitor failed (${exit.code}): ${stderr}`);readyReject(error);doneReject(error);}
  }).catch(error=>{readyReject(error);doneReject(error);});
  await ready;
  return {done,stop:async()=>{if(child.exitCode===null)child.kill();await exited;}};
}
async function bridgeRequest(port,token,method,payload) {
  const response=await fetch(`http://127.0.0.1:${port}/${method==='GET'?'health':'downloads'}`,{
    method,headers:{Authorization:`Bearer ${token}`,...(payload?{'Content-Type':'application/json'}:{})},
    ...(payload?{body:JSON.stringify(payload)}:{}),signal:AbortSignal.timeout(10000),
  });
  if(!response.ok)throw new Error(`Owned QDM bridge replied HTTP ${response.status}`);
  return response.json();
}
async function startQdm(workdir) {
  const dataDir=path.join(workdir,'qdm-state'),downloadDir=path.join(workdir,'qdm-files');
  await fs.mkdir(downloadDir,{recursive:true});
  const bootstrap=new CliClient(binary,dataDir,downloadDir,['--bridge']);
  let integration;
  try {await bootstrap.settings({connections,max_concurrent:1,speed_limit_kib:0,close_to_tray:false});integration=await bootstrap.command('integration');}
  finally {await bootstrap.shutdown();}
  const child=spawn(binary,['--data-dir',dataDir,'--download-dir',downloadDir],{windowsHide:false,stdio:['ignore','pipe','pipe']});
  let stderr='';child.stderr.on('data',b=>stderr=(stderr+b).slice(-20000));child.stdout.resume();
  const exit=processExit(child);exit.catch(()=>{});
  let port=integration.port;
  await waitFor(async()=>{
    if(child.exitCode!==null)throw new Error(`QDM GUI exited: ${stderr}`);
    try{return (await bridgeRequest(port,integration.token,'GET')).name==='QDM';}catch{return false;}
  },'owned Slint GUI bridge ready',30000,100);
  await delay(1200);
  return {child,dataDir,downloadDir,port,
    async add(url,filename,expected){return bridgeRequest(port,integration.token,'POST',{url,filename,expected_sha256:expected,start:true});},
    async close(){
      if(child.exitCode!==null)return;
      await powershell(`$qdmProcess=Get-Process -Id ${child.pid} -ErrorAction Stop; [void]$qdmProcess.CloseMainWindow()`);
      const result=await Promise.race([exit,delay(15000).then(()=>null)]);
      if(!result){child.kill();await exit;throw new Error('Owned QDM GUI failed to close gracefully; its isolated state is retained.');}
      if(result.code!==0)throw new Error(`Owned QDM GUI exited ${result.code}: ${stderr}`);
    }};
}
async function referenceDownload(name,url,workdir) {
  const referencePath=path.join(workdir,`${name}-reference.bin`);
  const started=Date.now();
  const response=await fetch(url,{headers:{'Accept-Encoding':'identity'},signal:AbortSignal.timeout(timeoutMs)});
  if(!response.ok||!response.body)throw new Error(`Reference GET ${name}: HTTP ${response.status}`);
  const hash=crypto.createHash('sha256');let received=0;
  const inspect=new Transform({transform(chunk,_encoding,callback){received+=chunk.length;hash.update(chunk);callback(null,chunk);}});
  await pipeline(Readable.fromWeb(response.body),inspect,syncFs.createWriteStream(referencePath,{flags:'wx'}));
  const declared=Number(response.headers.get('content-length'));
  if(declared)assert.equal(received,declared,'Reference length differs from Content-Length');
  return {name,url:response.url,bytes:received,sha256:hash.digest('hex'),etag:response.headers.get('etag'),lastModified:response.headers.get('last-modified'),referencePath,referenceElapsedMs:Date.now()-started};
}
function summarize(report) {
  const median=values=>{if(!values.length||!values.every(Number.isFinite))return null;const v=[...values].sort((a,b)=>a-b);return v.length%2?v[(v.length-1)/2]:(v[v.length/2-1]+v[v.length/2])/2;};
  const applications=['IDM','QDM'];
  const plannedScenarios=report.plannedScenarios||[];
  const scenarios=[...new Set([...plannedScenarios,...report.runs.map(row=>row.scenario)])];
  const plannedOrder=report.order||[];
  const plannedCounts=Object.fromEntries(applications.map(app=>[app,plannedOrder.filter(value=>value===app).length]));
  const expectedSequence=plannedScenarios.flatMap(scenario=>plannedOrder.map(application=>({scenario,application})));
  const planBalanced=plannedScenarios.length>0&&plannedCounts.IDM>=2&&plannedCounts.IDM===plannedCounts.QDM;
  const planMatched=expectedSequence.length===report.runs.length&&expectedSequence.every((expected,index)=>
    report.runs[index].scenario===expected.scenario&&report.runs[index].application===expected.application);
  const stableExecutables=typeof report.binarySha256==='string'&&typeof report.idmBinarySha256==='string'&&
    report.binarySha256After===report.binarySha256&&report.idmBinarySha256After===report.idmBinarySha256;
  const complete=report.status==='passed'&&Boolean(report.finishedAt)&&planMatched&&
    report.runs.every(row=>row.status==='passed'&&Number.isFinite(row.dispatchToReadyMs)&&row.dispatchToReadyMs>0);
  const eligible=!report.smokeOnly&&planBalanced&&complete&&stableExecutables;
  const ineligibleReasons=[];
  if(report.smokeOnly)ineligibleReasons.push('Smoke run; not a comparative series');
  if(!planBalanced)ineligibleReasons.push('A recorded balanced plan with at least2 runs per application is required');
  if(!planMatched)ineligibleReasons.push('Attempted runs do not match the complete recorded scenario/order plan');
  if(!complete)ineligibleReasons.push('The entire planned series has not finished with every run passing');
  if(!stableExecutables)ineligibleReasons.push('Final unchanged-executable hashes have not been verified');
  return scenarios.map(scenario=>{
    const groups={};
    for(const application of applications){
      const attempted=report.runs.filter(row=>row.scenario===scenario&&row.application===application);
      const rows=attempted.filter(row=>row.status==='passed');
      const failed=attempted.filter(row=>row.status==='failed').length;
      const planned=plannedScenarios.includes(scenario)?plannedCounts[application]:0;
      const readyTimes=rows.map(row=>row.dispatchToReadyMs);
      groups[application]={
        statisticsScope:'Successful runs only; failed and unattempted runs remain in the counts below',
        plannedRuns:planned,attemptedRuns:attempted.length,successfulRuns:rows.length,failedRuns:failed,
        unfinishedRuns:attempted.length-rows.length-failed,notRun:Math.max(0,planned-attempted.length),
        unexpectedRuns:Math.max(0,attempted.length-planned),runs:rows.length,
        medianDispatchToReadyMs:median(readyTimes),
        dispatchToReadyRangeMs:readyTimes.length&&readyTimes.every(Number.isFinite)?{min:Math.min(...readyTimes),max:Math.max(...readyTimes)}:null,
        medianTransferMiBps:median(rows.map(row=>row.throughputMiBps)),
        medianFirstRequestToReadyMs:median(rows.map(row=>row.firstRequestToReadyMs)),
        medianCpuMs:median(rows.map(row=>row.cpuMs)),
        peakWorkingSet:rows.length?Math.max(...rows.map(row=>row.peakWorkingSet)):null,
        peakPrivateBytes:rows.length?Math.max(...rows.map(row=>row.peakPrivateBytes)):null,
      };
    }
    const ratio=eligible?groups.IDM.medianDispatchToReadyMs/groups.QDM.medianDispatchToReadyMs:null;
    return {scenario,comparisonEligible:eligible,comparisonStatus:eligible?'complete-balanced-series':'descriptive-only',
      comparisonIneligibleReasons:ineligibleReasons,groups,qdmSpeedRatioToIdm:ratio,
      qdmReadyTimeDifferencePercent:ratio===null?null:(groups.QDM.medianDispatchToReadyMs/groups.IDM.medianDispatchToReadyMs-1)*100};
  });
}

module.exports={summarize};
if(require.main===module){
(async()=>{
  assert.equal(process.platform,'win32','This benchmark requires the user Windows machine.');
  assert.ok(Number.isInteger(idmPid)&&idmPid>0,'Pass --idm-pid for the existing IDM process.');
  assert.ok([32,64].includes(connections),'QDM connections must be32or64; IDMsettingread separately.');
  assert.ok(Number.isInteger(betweenRunsMs)&&betweenRunsMs>=0&&betweenRunsMs<=120000,'Invalid --between-runs-ms');
  assert.ok(order.length&&order.every(v=>['IDM','QDM'].includes(v)),'Invalid --order');
  await fs.mkdir(workParent,{recursive:true});await fs.mkdir(path.dirname(reportPath),{recursive:true});
  const workdir=await fs.mkdtemp(path.join(workParent,'qdm-idm-benchmark-'));
  const report={schema:1,suite:'Actual IDM and QDM Slint GUI sequential download benchmark',startedAt:new Date().toISOString(),smokeOnly:smoke,
    binary,binarySha256:await fileHash(binary),idmBinary:idm,idmBinarySha256:await fileHash(idm),idmProcessId:idmPid,qdmConnections:connections,
    idmConnectionSetting:'Must be separately evidenced from the installed program; runner does not modify it',
    platform:process.platform,os:os.release(),logicalProcessors:os.cpus().length,order,
    plannedScenarios:requestedScenario==='all'?['capped','fast',...Object.keys(publicUrls)]:[requestedScenario],
    betweenRunsMs,workdir,runs:[],references:[],idmPreflights:[],
    method:{qdm:'Actual Slint GUI with isolated profile; authenticated own bridge adds jobs; startup excluded',idm:'Existing installed IDM; /d URL /p ownfolder /f uniquename /n; no /s or /q',
      ready:'Exact expected final size and exclusive read access, confirmed on3polls; first successful poll timestamp used (requested50ms poll interval; actual timestamps retained)',
      hash:'Independent SHA-256 after timed completion on both; QDM internal pre-publication SHA-256 remains enabled',
      controlledNetwork:'Origin timestamps are HTTP write-to-socket observations, not a packet capture; actual overlapping body socket count includes initial non-Range GET',
      cappedScenario:'Requests a16KiBchunk followed by8mstimer delay perbodyconnection. ActualWindows timer cadence is measured, not assumed to provide an exact2MiB/s cap.',
      publicNetwork:'Public origin last-byte/TCP count are not directly observed; final-file readiness is timed',
      resources:'Only the specified main process PID is sampled; IDM helper/launcher processes are excluded. CPU interval starts at monitor readiness before dispatch and ends at file-ready confirmation, about100ms after first-ready observation. Memory maxima are sampled values at the requested50ms cadence, not lifetime or sub-poll peaks.',
      statistics:'Group medians/ranges describe successful runs only. Planned/failed/unfinished/not-run counts are always retained. Headline speed ratios require a finalized, unchanged-executable, completely passing balanced plan with at least2 runs per application.',
      idmPreflight:'Before each timed run, bind to the existing IDM PID/executable hash, detect registration reminders without clicking them, and close only Download complete dialogs whose exact saved path matches a previously hash-verified IDM result in this report. Other dialogs are left unchanged. A blocked preflight creates no timed transfer row.',
      order:'The recorded interleaved order limits simple time-order bias; perapplication counts are in the summary and a small sample is not a statistical guarantee',
      preservation:'No IDM registry/preferences/history modifications or queue start. Benchmark files/history entries have unique names and are retained for review.'}};
  let qdm,origin;
  const persist=async()=>{report.summary=summarize(report);await fs.writeFile(reportPath,JSON.stringify(report,null,2)+'\n');};
  try{
    const idmInfo=JSON.parse(await powershell(`$qdmProcess=Get-Process -Id ${idmPid} -ErrorAction Stop; @{id=$qdmProcess.Id;name=$qdmProcess.ProcessName;path=$qdmProcess.Path;startTime=$qdmProcess.StartTime.ToUniversalTime().ToString('o')} | ConvertTo-Json -Compress`));
    assert.equal(idmInfo.name.toLowerCase(),'idman','Specified process is not IDM');report.idmProcess=idmInfo;
    qdm=await startQdm(workdir);report.qdmProcessId=qdm.child.pid;
    const scenarios=report.plannedScenarios;
    for(const scenario of scenarios){
      assert.ok(['capped','fast',...Object.keys(publicUrls)].includes(scenario),'Unknown scenario');
      let reference;
      if(publicUrls[scenario]){
        reference=await referenceDownload(scenario,publicUrls[scenario],workdir);
        const pinned=publicPinnedHashes[scenario];
        if(pinned){
          reference.officialExpectedSha256=pinned.sha256;
          reference.officialDownloadPage=pinned.downloadPage;
          assert.equal(reference.sha256,pinned.sha256,'Independent reference did not match the pinned official SHA-256');
        }
        report.references.push(reference);await persist();
      }
      else if(!origin)origin=await createBenchmarkOrigin(bytes);
      for(const application of order){
        const index=report.runs.length+1;
        const preflight=await idmPreflight(report,workdir,index);
        report.idmPreflights.push({beforeRunIndex:index,scenario,nextApplication:application,...preflight});
        await persist();
        if(preflight.status!=='passed')throw new Error(`IDM preflight ${preflight.status}: ${preflight.error||'see preflight report'}`);
        const runId=`${Date.now()}-${index}-${crypto.randomBytes(4).toString('hex')}`;
        const filename=`QDM-IDM-BENCH-${scenario}-${application}-${runId}.bin`;
        const directory=application==='QDM'?qdm.downloadDir:path.join(workdir,'idm-files');await fs.mkdir(directory,{recursive:true});
        const file=path.join(directory,filename);
        let controlled=null,url,expectedBytes,expectedHash;
        if(reference){const u=new URL(reference.url);u.searchParams.set('qdm_benchmark',runId);url=u.toString();expectedBytes=reference.bytes;expectedHash=reference.sha256;}
        else {controlled=origin.createRun(runId,scenario==='capped'?8:0);url=controlled.url;expectedBytes=origin.bytes;expectedHash=origin.expectedSha256;}
        const row={index,scenario,application,url,filename,file,expectedBytes,expectedSha256:expectedHash,status:'running'};
        report.runs.push(row);await persist();console.log(`RUN ${index} ${scenario} ${application}`);
        const observation=await monitor(application==='QDM'?qdm.child.pid:idmPid,file,expectedBytes,workdir,index);
        let launched;
        try{
          row.dispatchAt=Date.now();
          if(application==='QDM'){
            const accepted=await qdm.add(url,filename,expectedHash);assert.equal(accepted.accepted,1);row.taskId=accepted.ids[0];
          }else{
            const child=spawn(idm,['/d',url,'/p',directory,'/f',filename,'/n'],{windowsHide:true,stdio:'ignore'});
            launched=processExit(child);launched.catch(()=>{});
          }
          row.dispatchAcknowledgedAt=Date.now();
          const observed=await observation.done;
          Object.assign(row,{firstFileReadyAt:observed.completed.firstReadyAt,readyConfirmedAt:observed.completed.confirmedAt,
            dispatchToReadyMs:observed.completed.firstReadyAt-row.dispatchAt,
            resourceObservationMs:observed.completed.at-observed.baseline.at,
            cpuMs:observed.completed.cpuMs-observed.baseline.cpuMs,
            peakWorkingSet:Math.max(observed.baseline.workingSet,...observed.samples.map(v=>v.workingSet)),
            peakPrivateBytes:Math.max(observed.baseline.privateBytes,...observed.samples.map(v=>v.privateBytes)),
            resourceSamples:observed.samples,resourceBaseline:observed.baseline});
          if(launched)row.idmLauncherExit=await Promise.race([launched,delay(2000).then(()=>({state:'still-running'}))]);
          row.actualSha256=await fileHash(file);assert.equal(row.actualSha256,expectedHash,'Independent SHA-256 mismatch');
          assert.equal((await fs.stat(file)).size,expectedBytes);
          if(controlled){await waitFor(()=>controlled.run.active.size===0,'controlled origin body requests finished',10000,20);row.origin=origin.snapshot(controlled.run);assert.equal(row.origin.uniqueByteCoverage,expectedBytes,'Origin did not cover exactly all file bytes');row.firstRequestToReadyMs=row.firstFileReadyAt-row.origin.firstRequestAt;row.dispatchToFirstRequestMs=row.origin.firstRequestAt-row.dispatchAt;row.originSendSpanMs=row.origin.lastOriginSendAt-row.origin.firstOriginSendAt;}
          else {row.origin=null;row.firstRequestToReadyMs=null;row.dispatchToFirstRequestMs=null;row.originSendSpanMs=null;}
          row.throughputMiBps=expectedBytes/(1024*1024)/(row.dispatchToReadyMs/1000);
          if(application==='QDM'){
            const saved=JSON.parse(await fs.readFile(path.join(qdm.dataDir,'state-v2.json'),'utf8'));
            const task=saved.tasks.find(value=>value.view.id===row.taskId);row.qdmTask=task?.view||null;
          }
          row.status='passed';console.log(`PASS ${index} ${application} ${row.dispatchToReadyMs}ms ${row.throughputMiBps.toFixed(2)}MiB/s hash verified`);
        }catch(error){row.status='failed';row.error=error.stack||String(error);await persist();throw error;}
        finally{await observation.stop();}
        await persist();await delay(betweenRunsMs);
      }
    }
    report.status='passed';
  }catch(error){report.status='failed';report.error=error.stack||String(error);process.exitCode=1;console.error(report.error);}
  finally{
    if(qdm){try{await qdm.close();}catch(error){report.qdmCloseError=String(error);report.status='failed';process.exitCode=1;}}
    if(origin)await origin.close();
    report.binarySha256After=await fileHash(binary);report.idmBinarySha256After=await fileHash(idm);
    if(report.binarySha256After!==report.binarySha256||report.idmBinarySha256After!==report.idmBinarySha256){report.status='failed';report.binaryChanged=true;process.exitCode=1;}
    report.finishedAt=new Date().toISOString();await persist();console.log(JSON.stringify({status:report.status,report:reportPath,workdir,summary:report.summary},null,2));
  }
})().catch(error=>{console.error(error.stack||error);process.exitCode=1;});
}
