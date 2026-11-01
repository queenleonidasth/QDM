#!/usr/bin/env node
'use strict';
// Real release GUI renderer probe. No production profile is opened or edited.
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { spawn } = require('node:child_process');
const { CliClient, fileHash, delay } = require('./helpers/cli-client.cjs');
const { createHttpFixture, MiB } = require('./helpers/http-fixture.cjs');
function option(name, fallback) {
  const i = process.argv.indexOf(name);
  if (i < 0) return fallback;
  if (!process.argv[i + 1] || process.argv[i + 1].startsWith('--')) throw new Error(`${name} requires a value.`);
  return process.argv[i + 1];
}
const binary = path.resolve(option('--binary', 'target/release/QDM.exe'));
const output = path.resolve(option('--report', 'docs/evidence/slint-idm-20261027/render-performance.json'));
const backend = option('--backend', 'winit-femtovg');
const selectedMode = option('--mode', 'all');
if (!['all', 'full', 'idle'].includes(selectedMode)) throw new Error('--mode must be all, full, or idle.');
function bounded(promise, milliseconds, message) {
  let timeout;
  return Promise.race([
    promise,
    new Promise((_, reject) => {timeout = setTimeout(() => reject(new Error(message)), milliseconds);}),
  ]).finally(() => clearTimeout(timeout));
}
async function closeGui(child, exited) {
  if (child.exitCode !== null) return;
  const stopper = spawn('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', `$p=Get-Process -Id ${child.pid} -ErrorAction Stop; [void]$p.CloseMainWindow()`], {windowsHide: true, stdio: 'ignore'});
  await new Promise(resolve => stopper.once('exit', resolve));
  let done;
  try { done = await bounded(exited, 15000, 'Owned Slint GUI did not exit gracefully.'); }
  catch (error) { child.kill(); await bounded(exited, 5000, 'Owned Slint GUI did not terminate.'); throw error; }
  if (done.code !== 0) throw new Error(`Slint GUI exited with ${done.code}.`);
}
async function sample(mode, dataDir, downloadDir, milliseconds, hidden=false) {
  const lines = [];
  const started = Date.now();
  const child = spawn(binary, ['--data-dir', dataDir, '--download-dir', downloadDir], {
    windowsHide: false, stdio: ['ignore', 'pipe', 'pipe'],
    env: {...process.env, SLINT_BACKEND:backend, SLINT_DEBUG_PERFORMANCE: `${mode},console`},
  });
  let stdout = '', stderr = '', buffer = '';
  child.stdout.on('data', data => {stdout += data.toString();});
  child.stderr.on('data', data => {
    const chunk = data.toString(); stderr += chunk; buffer += chunk;
    const parts = buffer.split(/\r?\n/); buffer = parts.pop();
    for (const line of parts) {
      const atUnixMs=Date.now();
      lines.push({atMs:atUnixMs-started, atUnixMs, text:line});
    }
  });
  const exited = new Promise((resolve, reject) => {child.once('error', reject); child.once('exit', (code,signal) => resolve({code,signal}));});
  const resourcePath=path.join(path.dirname(dataDir),`${mode}-${hidden?'hidden':'visible'}-resources.json`);
  const sampler=spawn('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'slint-resource-sample.ps1'),'-TargetProcessId',String(child.pid),'-OutputPath',resourcePath,'-Label',`Slint ${backend} ${mode}, ${hidden?'hidden':'visible'}`,...(hidden?['-HideWindow']:[])],{windowsHide:true,stdio:['ignore','ignore','pipe']});
  let samplerError='';sampler.stderr.on('data',b=>samplerError+=b);
  const sampled=new Promise(resolve=>{sampler.once('error',e=>resolve({code:-1,error:String(e)}));sampler.once('exit',code=>resolve({code,error:samplerError}));});
  let resourceResult;
  try {
    const resourceDone=bounded(sampled,35000,'GUI resource sampling exceeded its 35-second bound.').then(value=>{
      if(value.code!==0)throw new Error(`GUI resource sampling failed: ${value.error}`);
      return value;
    });
    // The resource sampler owns the hide/restore interval. Never close its
    // target while it is still measuring. A hidden run ends when that sampler
    // finishes, so a restored visible tail is not part of the intended sample.
    const measurement=Promise.all([hidden?Promise.resolve():delay(milliseconds),resourceDone]);
    const unexpectedExit=exited.then(result=>{throw new Error(`GUI exited during measurement: ${JSON.stringify(result)} ${stderr.slice(-3000)}`);});
    [,resourceResult]=await Promise.race([measurement,unexpectedExit]);
  } finally {
    // On timeout/error terminate only our sampler before closing only our GUI.
    try {
      if(sampler.exitCode===null){sampler.kill();await bounded(sampled,5000,'Owned resource sampler did not terminate.');}
    } finally { await closeGui(child, exited); }
  }
  const resources=JSON.parse((await fs.readFile(resourcePath,'utf8')).replace(/^\uFEFF/,''));
  const frames = lines.flatMap(line => {
    const match = /([\d.]+)\s*(?:frames per second|fps)\b/i.exec(line.text) || /(?:frames per second|fps)\s*[:=]\s*([\d.]+)/i.exec(line.text);
    return match ? [{atMs:line.atMs,atUnixMs:line.atUnixMs,fps:Number(match[1]),line:line.text}] : [];
  });
  // Slint's FPS log contains a reporting timestamp, not exact frame-interval
  // boundaries. Do not report a hidden FPS statistic from an interval that
  // may straddle native hide/restore. Raw logs and native visibility times are
  // retained; hidden CPU and memory remain directly measured.
  const warm = hidden?[]:frames.filter(row => row.atMs >= 5000);
  const values = warm.map(row=>row.fps).sort((a,b)=>a-b);
  const middle=Math.floor(values.length/2);
  return {mode,hidden,resources,pid:child.pid,durationMs:Date.now()-started,frameSamples:frames,
    frameTelemetryStatus:hidden?'not_aggregated_for_native_hidden_window':values.length?'measured':'unavailable',
    frameTelemetryNote:hidden?'Hidden FPS is intentionally unavailable: log reporting windows may cross hide/restore. Use windowObservation for the verified native hidden interval and resources for CPU/memory.':null,
    warmupMs:5000, measuredSamples:values.length,
    medianFps:values.length?(values.length%2?values[middle]:(values[middle-1]+values[middle])/2):null,
    meanFps:values.length?values.reduce((a,b)=>a+b,0)/values.length:null,
    minFps:values[0]??null,maxFps:values.at(-1)??null,stdout,stderr};
}
async function main() {
  if (process.platform !== 'win32') throw new Error('This acceptance measures the actual Windows Slint GUI.');
  const workspace = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-slint-render-'));
  const dataDir=path.join(workspace,'state'), downloadDir=path.join(workspace,'files');
  await fs.mkdir(downloadDir,{recursive:true});
  const report={schema:2,suite:'Slint release GUI renderer measurement',startedAt:new Date().toISOString(),binary,binarySha256:await fileHash(binary),workspace,requestedBackend:backend,requestedMode:selectedMode,
    interpretation:'Full-speed intentionally forces redraws and measures render capacity; lazy measures actual idle repaints. Neither is a frame-pacing percentile or a network benchmark.',samples:[]};
  const fixture = await createHttpFixture({sizeBytes:64*MiB,delayMs:1,chunkSize:64*1024});
  const cli=new CliClient(binary,dataDir,downloadDir);
  try {
    await cli.settings({connections:64,close_to_tray:false});
    const id=await cli.add({url:fixture.url,filename:'QDM-64-connection-render-fixture.bin',expected_sha256:fixture.hash()});
    const completed=await cli.terminal(id,90000);
    if(completed.status!=='completed') throw new Error(completed.error||'Fixture did not complete');
    if(await fileHash(completed.file_path)!==fixture.hash()) throw new Error('Fixture hash mismatch');
    report.fixture={sizeBytes:64*MiB,sha256:fixture.hash(),laneCount:completed.lanes.length,peakConnections:completed.peak_connections};
    await cli.shutdown();
    await fixture.close();
    if(selectedMode!=='idle') report.samples.push(await sample('refresh_full_speed',dataDir,downloadDir,20000));
    if(selectedMode!=='full') {
      report.samples.push(await sample('refresh_lazy',dataDir,downloadDir,15000));
      report.samples.push(await sample('refresh_lazy',dataDir,downloadDir,15000,true));
    }
    report.status=report.samples.filter(s=>!s.hidden).every(s=>s.measuredSamples>0)?'measured':'telemetry_unavailable';
  } catch(error) {report.status='failed';report.error=String(error.stack||error);process.exitCode=1;}
  finally {
    if(!cli.exited) await cli.shutdown().catch(()=>cli.crash());
    await fixture.close().catch(()=>{});
    report.finishedAt=new Date().toISOString();
    await fs.mkdir(path.dirname(output),{recursive:true});
    await fs.writeFile(output,JSON.stringify(report,null,2)+'\n');
    await fs.rm(workspace,{recursive:true,force:true,maxRetries:5,retryDelay:100});
  }
  console.log(JSON.stringify({status:report.status,report:output,backend,selectedMode,samples:report.samples.map(({mode,hidden,medianFps,meanFps,minFps,maxFps,measuredSamples})=>({mode,hidden,medianFps,meanFps,minFps,maxFps,measuredSamples})),error:report.error}));
}
main().catch(error=>{console.error(error.stack||error);process.exitCode=1;});
