#!/usr/bin/env node
'use strict';
// Capture only a QDM HWND created by this script. No desktop recording, global
// input injection, foreground assumption, user profile, or production files.
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const readline = require('node:readline');
const {spawn} = require('node:child_process');
const {performance} = require('node:perf_hooks');
const {CliClient,waitFor,delay,fileHash} = require('./helpers/cli-client.cjs');
const {createHttpFixture,MiB,KiB} = require('./helpers/http-fixture.cjs');

function option(name,fallback) {
  const i=process.argv.indexOf(name);
  if(i<0)return fallback;
  if(!process.argv[i+1]||process.argv[i+1].startsWith('--'))throw new Error(`${name} requires a value.`);
  return process.argv[i+1];
}
const binary=path.resolve(option('--binary','target/release/QDM.exe'));
const ffmpeg=path.resolve(option('--ffmpeg','C:\\Users\\QUEEN\\AppData\\Local\\Microsoft\\WinGet\\Links\\ffmpeg.exe'));
const backend=option('--backend','winit-femtovg');
const output=path.resolve(option('--output-dir',`docs/evidence/slint-motion-${new Date().toISOString().replace(/[:.]/g,'-')}`));
const durationSeconds=16;

function bounded(promise,milliseconds,message) {
  let timer;
  return Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error(message)),milliseconds);})]).finally(()=>clearTimeout(timer));
}
function launch(file,args,options={}) {
  const child=spawn(file,args,{windowsHide:true,stdio:['pipe','pipe','pipe'],...options});
  let stdout='',stderr='';
  child.stdout.on('data',b=>{stdout=(stdout+b.toString('utf8')).slice(-100000);});
  child.stderr.on('data',b=>{stderr=(stderr+b.toString('utf8')).slice(-100000);});
  const exited=new Promise(resolve=>{child.once('error',error=>resolve({code:-1,error:String(error)}));child.once('exit',(code,signal)=>resolve({code,signal}));});
  return {child,exited,get stdout(){return stdout;},get stderr(){return stderr;}};
}
class UiaClient {
  constructor(processId) {
    this.process=launch('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'helpers','slint-motion-uia.ps1'),'-TargetProcessId',String(processId)]);
    this.pending=new Map();this.sequence=0;
    readline.createInterface({input:this.process.child.stdout}).on('line',line=>{
      let reply;try{reply=JSON.parse(line.replace(/^\uFEFF/,''));}catch{return;}
      const request=this.pending.get(reply.id);if(!request)return;
      clearTimeout(request.timer);this.pending.delete(reply.id);
      if(reply.ok)request.resolve(reply);else request.reject(new Error(reply.error||'UIA action failed.'));
    });
    this.process.exited.then(result=>{for(const request of this.pending.values()){clearTimeout(request.timer);request.reject(new Error(`Owned UIA helper exited: ${JSON.stringify(result)} ${this.process.stderr}`));}this.pending.clear();});
    this.process.child.stdin.on('error',()=>{});
  }
  command(action,fields={},timeoutMs=8000) {
    const id=++this.sequence;
    return new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>{this.pending.delete(id);reject(new Error(`UIA ${action} timed out. ${this.process.stderr}`));},timeoutMs);
      this.pending.set(id,{resolve,reject,timer});
      this.process.child.stdin.write(JSON.stringify({id,action,...fields})+'\n');
    });
  }
  async close() {
    this.process.child.stdin.end();
    try{await bounded(this.process.exited,3000,'Owned UIA helper did not stop.');}
    catch{this.process.child.kill();await bounded(this.process.exited,3000,'Owned UIA helper did not terminate.');}
  }
}

async function main() {
  if(process.platform!=='win32')throw new Error('Native motion capture requires Windows.');
  await fs.access(binary);await fs.access(ffmpeg);await fs.mkdir(output,{recursive:true});
  const workspace=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-native-motion-'));
  const dataDir=path.join(workspace,'state'),downloadDir=path.join(workspace,'downloads');
  await fs.mkdir(downloadDir,{recursive:true});
  const reportPath=path.join(output,'motion-capture.json');
  const report={schema:1,suite:'Actual Slint portrait motion capture',startedAt:new Date().toISOString(),binary,binarySha256:await fileHash(binary),requestedBackend:backend,ffmpeg,outputDirectory:output,workspace,dataDir,downloadDir,
    captureMethod:'FFmpeg gdigrab scoped to exact verified owned HWND at 30 fps; PrintWindow(PW_RENDERFULLCONTENT) stills; no desktop capture or foreground requirement.',
    fixtureDescription:'Deterministic local byte fixtures with real transfers, completed-file hashes, live pause/resume and real UIA controls. Demo names identify fixture files, not real archives.',
    timestampMeaning:'UIA action times are wall-clock timestamps at invocation. Video-relative estimates subtract FFmpeg process start; first-frame observation/progress is retained to expose startup uncertainty. This is a visual review, not a frame-pacing benchmark.',
    retainedWorkspace:true,visualInspectionRequired:true,fixtures:[],actions:[],screenshots:[],observedApplicationWarnings:[]};
  let bootstrap,fastFixture,liveFixture,gui,uia,recorder,liveId,recordingStart=0;
  const writeReport=()=>fs.writeFile(reportPath,JSON.stringify(report,null,2)+'\n');
  function rememberWarnings(controls,stage) {
    for(const control of controls||[]) {
      if(/could not save download progress|access is denied/i.test(control.name||'')&&!report.observedApplicationWarnings.some(w=>w.text===control.name)) {
        report.observedApplicationWarnings.push({text:control.name,stage,observedAtUnixMs:Date.now()});
      }
    }
  }
  const task=async()=>{
    try{const saved=JSON.parse(await fs.readFile(path.join(dataDir,'state-v2.json'),'utf8'));return saved.tasks.find(t=>t.view.id===liveId)?.view;}catch{return null;}
  };
  const ensureGui=()=>{if(gui.child.exitCode!==null)throw new Error(`Owned GUI exited: ${gui.stderr}`);};
  const waitTask=(predicate,description,timeoutMs=6000)=>waitFor(async()=>{ensureGui();const value=await task();if(value&&['error','failed'].includes(value.status))throw new Error(value.error||'Live fixture failed');return value&&predicate(value)?value:null;},description,timeoutMs,60);
  async function screenshot(name) {
    const target=path.join(output,`${name}.png`);
    const startedAtUnixMs=Date.now();
    const process=launch('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'capture-window.ps1'),'-TargetProcessId',String(gui.child.pid),'-OutputPath',target]);
    let result;
    try{result=await bounded(process.exited,7000,'Owned-window PrintWindow capture timed out.');}
    catch(error){process.child.kill();await process.exited;throw error;}
    if(result.code!==0)throw new Error(`PrintWindow failed: ${process.stderr}`);
    const metadata=JSON.parse(process.stdout.replace(/^\uFEFF/,'').trim());
    const entry={name,...metadata,startedAtUnixMs,completedAtUnixMs:Date.now(),sha256:await fileHash(target)};
    report.screenshots.push(entry);await writeReport();return entry;
  }
  async function invoke(name,scheduledMs=null) {
    ensureGui();const requestedAtUnixMs=Date.now();
    const reply=await uia.command('invoke',{name});
    const entry={name,scheduledMs,requestedAtUnixMs,...reply.result,completedAtUnixMs:reply.completedAtUnixMs,
      estimatedVideoMs:recordingStart?reply.result.invokedAtUnixMs-recordingStart:null};
    report.actions.push(entry);
    await fs.appendFile(path.join(output,'actions.ndjson'),JSON.stringify(entry)+'\n');
    await writeReport();return entry;
  }
  try {
    // Complete two honest history fixtures before the film; keep the real live
    // fixture paused until UIA selects and resumes it in the actual GUI.
    fastFixture=await createHttpFixture({sizeBytes:8*MiB,delayMs:1,chunkSize:64*KiB});
    liveFixture=await createHttpFixture({sizeBytes:96*MiB,delayMs:120,chunkSize:8*KiB});
    bootstrap=new CliClient(binary,dataDir,downloadDir);
    await bootstrap.settings({connections:64,max_concurrent:1,close_to_tray:false,speed_limit_kib:0});
    // The current queue presents oldest first. Create the paused live item
    // first so its real row is visible even at the narrow portrait minimum.
    const liveName='Queens-collection-demo.bin';
    liveId=await bootstrap.add({url:liveFixture.url,filename:liveName,expected_sha256:liveFixture.hash(),start:false});
    for(const filename of ['Royal-workspace-demo.bin','Portrait-library-demo.bin']) {
      const id=await bootstrap.add({url:fastFixture.url,filename,expected_sha256:fastFixture.hash()});
      const completed=await bootstrap.terminal(id,30000);
      if(completed.status!=='completed')throw new Error(completed.error||'History fixture did not complete.');
      const actualSha256=await fileHash(completed.file_path);
      if(actualSha256!==fastFixture.hash())throw new Error('History fixture independent hash mismatch.');
      report.fixtures.push({id,filename,bytes:8*MiB,sha256:actualSha256,status:'completed',file:completed.file_path});
    }
    report.fixtures.push({id:liveId,filename:liveName,bytes:96*MiB,expectedSha256:liveFixture.hash(),status:'paused_for_UI_start'});
    await bootstrap.shutdown();await fastFixture.close();fastFixture=null;
    await fs.writeFile(path.join(dataDir,'ui-settings.json'),JSON.stringify({reduced_motion:false})+'\n');
    const guiEnv={...process.env,SLINT_BACKEND:backend};
    delete guiEnv.SLINT_DEBUG_PERFORMANCE;delete guiEnv.QDM_TEST_MODE;delete guiEnv.QDM_TEST_ASSEMBLY_DELAY_MS;
    gui=launch(binary,['--data-dir',dataDir,'--download-dir',downloadDir],{windowsHide:false,env:guiEnv});
    uia=new UiaClient(gui.child.pid);
    report.windowBeforePlacement=(await uia.command('window',{},17000)).result;
    report.windowPlacement=(await uia.command('place_within_work_area')).result;
    report.window=(await uia.command('window')).result;
    if(!report.window.visible||report.window.processId!==gui.child.pid)throw new Error('The capture window is not the visible, owned QDM process.');
    if(!report.window.fullyInsideWorkArea)throw new Error('The owned capture window is not fully inside its monitor work area.');
    await uia.command('select',{name:liveName});
    await invoke('Resume');
    const initial=await waitTask(t=>t.status==='downloading'&&t.downloaded_bytes>MiB,'actual live download before capture',12000);
    report.initialLive={downloadedBytes:initial.downloaded_bytes,lanes:initial.lanes.length,connectionsActive:initial.connections_active};
    report.initialControls=(await uia.command('inventory')).result;
    rememberWarnings(report.initialControls,'before_recording');
    await screenshot('01-live-main');

    const videoPath=path.join(output,'portrait-motion.mp4');
    const ffmpegArgs=['-hide_banner','-loglevel','warning','-nostdin','-y','-f','gdigrab','-framerate','30','-draw_mouse','0','-i',`hwnd=${report.window.hwnd}`,'-t',String(durationSeconds),'-an','-vf','pad=ceil(iw/2)*2:ceil(ih/2)*2','-c:v','libx264','-preset','veryfast','-crf','18','-pix_fmt','yuv420p','-movflags','+faststart','-progress','pipe:1','-stats_period','0.25',videoPath];
    recordingStart=Date.now();
    recorder=launch(ffmpeg,ffmpegArgs,{stdio:['ignore','pipe','pipe']});
    report.video={path:videoPath,args:ffmpegArgs,frameRate:30,requestedSeconds:durationSeconds,processStartedAtUnixMs:recordingStart,progress:[]};
    let firstFrameResolve;
    const firstFrame=new Promise(resolve=>{firstFrameResolve=resolve;});
    let progress={};
    readline.createInterface({input:recorder.child.stdout}).on('line',line=>{
      const separator=line.indexOf('=');if(separator<0)return;
      progress[line.slice(0,separator)]=line.slice(separator+1);
      if(line.startsWith('progress=')) {
        const observation={...progress,observedAtUnixMs:Date.now()};report.video.progress.push(observation);
        if(Number(progress.frame)>0&&!report.video.firstFrameObservedAtUnixMs){report.video.firstFrameObservedAtUnixMs=observation.observedAtUnixMs;firstFrameResolve(observation);}
        progress={};
      }
    });
    await bounded(Promise.race([firstFrame,recorder.exited.then(result=>{throw new Error(`Window-only FFmpeg capture exited before its first frame: ${JSON.stringify(result)} ${recorder.stderr}`);})]),7000,'FFmpeg did not capture a first owned-window frame.');
    const timelineStarted=performance.now();
    report.video.interactionTimelineStartedAtUnixMs=Date.now();
    const at=async(milliseconds,action)=>{await delay(Math.max(0,milliseconds-(performance.now()-timelineStarted)));ensureGui();if(recorder.child.exitCode!==null)throw new Error('Recording ended before the interaction timeline.');await action();};
    await at(650,()=>invoke('Settings',650));
    await at(1100,()=>screenshot('02-settings'));
    await at(2700,()=>invoke('Close settings',2700));
    await at(3400,()=>invoke('Connection details',3400));
    await at(3900,()=>screenshot('03-connections'));
    await at(5400,()=>invoke('Close connection details',5400));
    let paused,resumeInvocation;
    await at(6200,async()=>{await invoke('Pause',6200);paused=await waitTask(t=>t.status==='paused','real UI pause checkpoint');report.pauseCheckpoint={downloadedBytes:paused.downloaded_bytes,status:paused.status};});
    await at(6900,()=>screenshot('04-paused'));
    // Real resume validation can sample many saved pieces before body bytes
    // advance. Record the actual invocation, then let its work continue while
    // the UI filter motions are captured; verification follows the film.
    await at(8300,async()=>{resumeInvocation=await invoke('Resume',8300);});
    await at(10100,()=>invoke('Active downloads',10100));
    await at(11200,()=>invoke('Done downloads',11200));
    await at(11600,()=>screenshot('05-finished-filter'));
    await at(13400,()=>invoke('All downloads',13400));
    report.video.interactionTimelineFinishedAtUnixMs=Date.now();
    const recordingResult=await bounded(recorder.exited,12000,'Owned FFmpeg capture did not finish.');
    report.video.exit=recordingResult;report.video.stderr=recorder.stderr;
    if(recordingResult.code!==0)throw new Error(`Window-only FFmpeg capture failed: ${recorder.stderr}`);
    const videoStat=await fs.stat(videoPath);if(videoStat.size<10000)throw new Error('The captured video is unexpectedly small.');
    report.video.bytes=videoStat.size;report.video.sha256=await fileHash(videoPath);
    report.video.timelineWithinRequestedDuration=report.video.interactionTimelineFinishedAtUnixMs-recordingStart<durationSeconds*1000;
    const resumeVerificationStarted=Date.now();
    const resumed=await waitTask(t=>t.downloaded_bytes>paused.downloaded_bytes&&['downloading','assembling','verifying','completed'].includes(t.status),'real UI resume advances bytes after recording',30000);
    report.resumeCheckpoint={downloadedBytes:resumed.downloaded_bytes,resumedBytes:resumed.resumed_bytes,status:resumed.status,invokedAtUnixMs:resumeInvocation.invokedAtUnixMs,verificationStartedAtUnixMs:resumeVerificationStarted,verifiedAtUnixMs:Date.now(),verificationTimeoutMs:30000};
    await screenshot('06-live-main');
    const completed=await waitTask(t=>t.status==='completed','live fixture completion after capture',45000);
    const actualSha256=await fileHash(completed.file_path);
    if(actualSha256!==liveFixture.hash())throw new Error('Live fixture independent SHA-256 mismatch.');
    Object.assign(report.fixtures.at(-1),{status:'completed',file:completed.file_path,sha256:actualSha256,laneCount:completed.lanes.length,peakConnections:completed.peak_connections});
    await delay(300);await screenshot('07-completed');
    report.finalControls=(await uia.command('inventory')).result;
    rememberWarnings(report.finalControls,'after_completion');
    await invoke('Exit QDM');
    const guiResult=await bounded(gui.exited,12000,'Owned QDM GUI did not exit cleanly.');
    if(guiResult.code!==0)throw new Error(`Owned QDM GUI exit failed: ${gui.stderr}`);
    report.status=report.observedApplicationWarnings.length?'captured_with_application_warnings':report.video.timelineWithinRequestedDuration?'captured_pending_visual_review':'captured_with_timeline_overrun';
    report.visualReview='Inspect PNGs and video for blank frames, clipping, overlapping text, Path strokes, gradients, rounded clipping, sheet opacity, and transitions before accepting the design.';
  } catch(error) {
    report.status='failed';report.error=String(error.stack||error);process.exitCode=1;
    if(uia){try{report.failureControls=(await uia.command('inventory')).result;rememberWarnings(report.failureControls,'failure');}catch{}}
    if(gui&&gui.child.exitCode===null){try{await screenshot('failure');}catch(captureError){report.failureCaptureError=String(captureError);}}
  } finally {
    if(recorder&&recorder.child.exitCode===null){recorder.child.kill();await bounded(recorder.exited,5000,'Owned recorder did not terminate.').catch(()=>{});}
    if(recorder){report.video=report.video||{};report.video.stderr=recorder.stderr;}
    if(gui&&gui.child.exitCode===null){
      try{if(uia)await uia.command('close');await bounded(gui.exited,8000,'Owned GUI close timed out.');}
      catch{gui.child.kill();await bounded(gui.exited,5000,'Owned GUI termination timed out.').catch(()=>{});report.forcedGuiCleanup=true;}
    }
    if(uia)await uia.close().catch(error=>{report.helperCleanupError=String(error);});
    if(bootstrap&&!bootstrap.exited)await bootstrap.shutdown().catch(()=>bootstrap.crash());
    if(fastFixture)await fastFixture.close().catch(()=>{});
    if(liveFixture){report.origin={url:liveFixture.url,metrics:liveFixture.metrics,requests:liveFixture.requests};await liveFixture.close().catch(()=>{});}
    if(gui)report.guiStderr=gui.stderr;
    report.finishedAt=new Date().toISOString();await writeReport();
  }
  console.log(JSON.stringify({status:report.status,report:reportPath,video:report.video?.path,screenshots:report.screenshots.map(s=>s.path),workspace,error:report.error}));
}
main().catch(error=>{console.error(error.stack||error);process.exitCode=1;});
