'use strict';
// Real Zen/Gecko native-messaging integration, isolated persistent QA profile.
// Never changes the user's active Zen profile and never creates downloads.
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const net = require('node:net');
const crypto = require('node:crypto');
const {spawn} = require('node:child_process');
const root = path.resolve(__dirname, '..');
const zen = 'C:\\Program Files\\Zen Browser\\zen.exe';
const driverExe = path.resolve(root, '..', '.tools', 'browser-qa', 'geckodriver.exe');
const xpi = path.join(root, 'release', 'QDM-Zen-AutoCapture-0.3.1.xpi');
const evidenceDir = path.join(root, 'docs', 'evidence', 'zen-autopair-20261027');
const extensionId = 'qdm@queen.local';
const report = {startedAt:new Date().toISOString(), status:'running', checks:[], scope:'Owned isolated Zen profile; production QDM bridge health only; no actual download queued.'};
let dir, profile, driver, driverPort, sid, exited=false, driverLog='';
const delay=ms=>new Promise(r=>setTimeout(r,ms));
function ok(name, extra={}){report.checks.push({name,passed:true,...extra});console.log('PASS '+name);}
async function until(fn,desc,ms=20000){
 const stop=Date.now()+ms;let last;
 while(Date.now()<stop){try{const x=await fn();if(x)return x;}catch(e){last=e;}await delay(150);}
 throw new Error('Timeout '+desc+(last?': '+last.message:''));
}
async function request(method, endpoint, body, ms=25000){
 const ctrl=new AbortController();const timeout=setTimeout(()=>ctrl.abort(),ms);
 try{
  const resp=await fetch('http://127.0.0.1:'+driverPort+endpoint,{method,headers:body===undefined?{}:{'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body),signal:ctrl.signal});
  const j=await resp.json();if(!resp.ok||j.value?.error)throw Error(endpoint+': '+(j.value?.message||j.value?.error||resp.status));
  return j.value;
 }finally{clearTimeout(timeout);}
}
const call=(method,endpoint,body,ms)=>request(method,'/session/'+sid+endpoint,body,ms);
const execute=(script,args=[])=>call('POST','/execute/sync',{script,args});
const asyncExecute=(script,args=[])=>call('POST','/execute/async',{script,args});
async function freePort(){const s=net.createServer();await new Promise((res,rej)=>{s.once('error',rej);s.listen(0,'127.0.0.1',res)});const p=s.address().port;await new Promise(r=>s.close(r));return p;}
async function begin(){
 driverPort=await freePort();exited=false;
 driver=spawn(driverExe,['--host','127.0.0.1','--port',String(driverPort),'--log','warn','--allow-system-access'],{windowsHide:true,stdio:['ignore','pipe','pipe']});
 driver.on('exit',()=>{exited=true;});
 for(const stream of [driver.stdout,driver.stderr])stream.on('data',x=>{if(driverLog.length<100000)driverLog+=x.toString();});
 await until(async()=>{if(exited)throw Error('geckodriver exited');return (await request('GET','/status',undefined,1500)).ready},'driver ready',15000);
 const session=await request('POST','/session',{capabilities:{alwaysMatch:{
  browserName:'firefox',
  'moz:firefoxOptions':{binary:zen,args:['-headless','-no-remote','-profile',profile],prefs:{
    'xpinstall.signatures.required':false,
    'browser.shell.checkDefaultBrowser':false,
    'browser.aboutwelcome.enabled':false,
    'browser.startup.page':0,
    'browser.startup.homepage':'about:blank',
    'app.update.auto':false,
    'datareporting.policy.dataSubmissionEnabled':false,
    'toolkit.telemetry.enabled':false
  },log:{level:'warn'}}
 }}},120000);
 sid=session.sessionId; if(!sid)throw Error('Missing webdriver session');
 await call('POST','/timeouts',{script:20000,pageLoad:20000,implicit:0});
}
async function end(){
 if(sid){try{await call('DELETE','',undefined,12000);}catch(e){report.deleteError=e.message;}sid=null;}
 if(driver&&!exited){await new Promise(done=>{const killer=spawn('taskkill.exe',['/PID',String(driver.pid),'/T','/F'],{windowsHide:true,stdio:'ignore'});killer.once('exit',done);killer.once('error',done);});}
 driver=null;await delay(750);
}
async function openOptions(){
 const old=new Set(await call('GET','/window/handles'));
 await call('POST','/moz/context',{context:'chrome'});
 let url;
 try{url=await execute(`
 const policy=WebExtensionPolicy.getByID(arguments[0]);
 if(!policy)throw Error('QDM addon not installed');
 const browserWindow=window.gBrowser?window:Services.wm.getMostRecentWindow('navigator:browser');
 const url=policy.getURL('options.html');browserWindow.gBrowser.selectedTab=browserWindow.gBrowser.addTrustedTab(url);return url;
 `,[extensionId]);}
 finally{await call('POST','/moz/context',{context:'content'});}
 const handle=await until(async()=> (await call('GET','/window/handles')).find(x=>!old.has(x)),'options tab');
 await call('POST','/window',{handle});
 await until(async()=>(await call('GET','/url'))===url,'options URL');
}
async function get(selector){return execute('return document.querySelector(arguments[0])?.value || "";',[selector]);}
async function text(selector){return execute('return document.querySelector(arguments[0])?.textContent || "";',[selector]);}
async function click(selector){
 const result=await call('POST','/element',{using:'css selector',value:selector});
 const id=result['element-6066-11e4-a52e-4f735466cecf'];
 await call('POST','/element/'+id+'/click',{});
}
async function store(patch){
 const result=await asyncExecute(`
 const cb=arguments[arguments.length-1];
 const w=window.wrappedJSObject||window;
 const api=w.browser||w.chrome||globalThis.browser||globalThis.chrome;
 Promise.resolve(api.storage.local.set(arguments[0])).then(()=>cb({ok:true}),e=>cb({ok:false,message:String(e)}));
 `,[patch]);
 if(!result.ok)throw Error('Cannot write isolated extension storage: '+result.message);
}
async function checkConnection(){
 await click('#check-connection');
 return until(async()=>{
  const message=await text('#message');
  if(message.includes('ไม่สำเร็จ'))throw Error(message);
  return message.includes('เชื่อมต่อ QDM อัตโนมัติแล้ว') && message;
 },'native pairing',16000);
}
(async()=>{
 await fs.mkdir(evidenceDir,{recursive:true});
 dir=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-zen-autopair-'));
 profile=path.join(dir,'profile');await fs.mkdir(profile);
 report.profileKind='isolated-test-only';
 for(const p of [zen,driverExe,xpi])await fs.access(p);
 const initialState=path.join(process.env.APPDATA,'QDM','state-v2.json');
 const before=crypto.createHash('sha256').update(await fs.readFile(initialState)).digest('hex');
 await begin();
 const installed=await call('POST','/moz/addon/install',{path:xpi,temporary:false});
 if(installed!==extensionId)throw Error('Wrong extension ID '+installed);
 ok('permanent_addon_installed_in_test_profile');
 await openOptions();
 if(await get('#token'))throw Error('New test profile was pre-paired unexpectedly');
 ok('fresh_profile_has_no_manual_pin');
 await checkConnection();
 const original=await get('#token');
 const port=Number(await get('#port'));
 if(!/^[a-f0-9]{64}$/i.test(original)||port<17876||port>17885)throw Error('Native messaging did not persist valid pair');
 ok('native_host_pairs_with_no_user_pin',{port});
 await store({port:17885});
 await checkConnection();
 if(Number(await get('#port'))!==port)throw Error('Port recovery failed');
 ok('changed_port_recovers_automatically');
 await store({token:''});
 await checkConnection();
 if((await get('#token'))!==original)throw Error('Lost token did not recover automatically');
 ok('missing_pin_recovers_automatically');
 await end();
 await begin();
 await openOptions();
 await until(async()=> (await get('#token'))===original, 'saved pairing token to populate options form after restart', 8000);
 ok('token_persisted_after_browser_restart');
 await checkConnection();
 if(Number(await get('#port'))!==port)throw Error('Native reconnection failed after browser restart');
 ok('automatic_reconnection_after_browser_restart');
 const after=crypto.createHash('sha256').update(await fs.readFile(initialState)).digest('hex');
 if(before!==after)throw Error('The production download state unexpectedly changed');
 ok('production_download_state_unchanged');
 report.status='passed';
})().catch(e=>{report.status='failed';report.error=e.message;process.exitCode=1;console.error(e.stack);}).finally(async()=>{
 await end().catch(e=>{report.cleanupError=e.message});
 report.finishedAt=new Date().toISOString();
 report.passed=report.status==='passed';report.failed=report.passed?0:1;
 await fs.writeFile(path.join(evidenceDir,'zen-autopair-restart.json'),JSON.stringify(report,null,2),'utf8');
 if(dir){await fs.rm(dir,{recursive:true,force:true,maxRetries:5,retryDelay:300}).catch(e=>{console.error('Profile cleanup:',e.message)});}
 console.log(JSON.stringify({status:report.status,checks:report.checks.length,failed:report.failed}));
});
