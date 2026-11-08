'use strict';
importScripts('session.js');
const api = globalThis.chrome;
const nativeHost = 'com.queen.qdm';
const defaults = { token: '', port: 17876, autoCapture: false, forwardSession: true };
const inflight = new Set();
let pairing = null;
const valid = cfg => /^[a-f0-9]{64}$/i.test(cfg?.token || '') && Number.isInteger(cfg?.port) && cfg.port >= 17876 && cfg.port <= 17885;
async function settings() { return api.storage.local.get(defaults); }
async function sessionHeaders(config, url, referer) {
  if (!config || config.forwardSession === false) return {};
  try { return await QDMSession.browserSession(url, referer); } catch (_) { return {}; }
}
function withSession(payload, headers) {
  return Object.keys(headers).length ? { ...payload, headers } : payload;
}
// onDeterminingFilename receives the final URL after redirects and blocks
// browser completion until suggest() is called. Do not use the old 400ms wait:
// for small downloads that delay exhausted the entire download lifetime.
function resolvedDownload(item) {
  const url = item.finalUrl || item.url;
  const mime = String(item.mime || '').toLowerCase();
  let parsed;
  try { parsed = new URL(url); } catch (_) { return null; }
  if (parsed.protocol !== 'https:' && parsed.protocol !== 'http:') return null;
  if (mime.startsWith('text/html')) return null;
  return {url, referrer: item.referrer || '', filename: item.filename || ''};
}
async function requestHealth(cfg) {
  if (!valid(cfg)) return false;
  try {
    const response = await fetch('http://127.0.0.1:' + cfg.port + '/health', {headers: {Authorization:'Bearer ' + cfg.token},signal:AbortSignal.timeout(1700)});
    return response.ok && (await response.json()).name === 'QDM';
  } catch {return false;}
}
async function discoverNative(start=true) {
  const response = await api.runtime.sendNativeMessage(nativeHost, {action:'discover',start});
  if (!response?.ok || !valid(response)) throw new Error(response?.error || 'QDM native host unavailable');
  await api.storage.local.set({token:response.token,port:response.port});
  return {...await settings(),token:response.token,port:response.port};
}
async function resolveConnection() {
  if (pairing) return pairing;
  pairing = (async () => {
    const cfg = await settings();
    if (await requestHealth(cfg)) return cfg;
    if (valid(cfg)) {
      for (let port=17876;port<=17885;port++) {
        if(port===cfg.port) continue;
        const probe={...cfg,port};
        if(await requestHealth(probe)){await api.storage.local.set({port});return probe;}
      }
    }
    return discoverNative(true);
  })().finally(()=>{pairing=null;});
  return pairing;
}
function eligible(item) {
  if(!Number.isInteger(item?.id)||(item.state && item.state!=='in_progress')||item.incognito||item.byExtensionId) return false;
  try {
    const url=new URL(item.url);
    if(!['http:','https:'].includes(url.protocol)||url.username||url.password) return false;
    if(url.hostname==='127.0.0.1' && Number(url.port)>=17876 && Number(url.port)<=17885) return false;
    return true;
  } catch{return false;}
}
function basename(value) {
  const v=typeof value==='string'?value.split(/[\\/]/).pop():'';
  return v&&v!=='.'&&v!=='..'?v:undefined;
}
async function handoff(payload) {
  let config=await resolveConnection();
  async function send(cfg) {
    const response=await fetch('http://127.0.0.1:'+cfg.port+'/downloads',{
      method:'POST',headers:{'Content-Type':'application/json',Authorization:'Bearer '+cfg.token},
      body:JSON.stringify(payload),signal:AbortSignal.timeout(10000)
    });
    if(response.status===401) {const e=new Error('QDM pairing expired');e.refreshPairing=true;throw e;}
    const data=await response.json();
    if(!response.ok || !data || data.accepted!==(Array.isArray(payload.urls)?payload.urls.length:1))throw new Error(data?.error||'QDM did not accept the download');
    return data;
  }
  try {return await send(config);}
  catch(e) {if(!e.refreshPairing)throw e;config=await discoverNative(true);return send(config);}
}
async function feedback(message,error=false){
  await api.storage.local.set({lastMessage:message});
  await api.action.setBadgeText({text:error?'!':'OK'});
  await api.action.setBadgeBackgroundColor({color:error?'#a84b3d':'#347f60'});
}
// The browser's copy is only cancelled after QDM proves it is receiving bytes.
// A Cloudflare challenge or an expired one-time link then leaves the browser's
// working download alone instead of losing both copies.
async function qdmStarted(paired, id, timeout=15000){
  if(!id)return false;
  const deadline=Date.now()+timeout;
  while(Date.now()<deadline){
    await new Promise(resolve=>setTimeout(resolve,500));
    try{
      const response=await fetch('http://127.0.0.1:'+paired.port+'/task-status?id='+encodeURIComponent(id),{
        headers:{Authorization:'Bearer '+paired.token},signal:AbortSignal.timeout(2500)
      });
      if(!response.ok)return false;
      const task=await response.json();
      if(task.status==='completed')return true;
      if(task.status==='downloading'&&Number(task.downloadedBytes)>0)return true;
      if(task.status==='error'||task.status==='failed')return false;
    }catch(_){return false;}
  }
  return false;
}
// The filename determination hook is a completion barrier: even 100KB files
// cannot be finalized by Dia while ownership is negotiated with QDM.
async function captureAtFilename(item) {
  if (!eligible(item) || inflight.has(item.id)) return false;
  inflight.add(item.id);
  let pausedByQdm = false;
  let captured = false;
  try {
    const config = await settings();
    if (!config.autoCapture) return false;
    const target = resolvedDownload(item);
    if (!target) return false;
    // Pausing avoids double network transfer for larger files. Some browsers
    // cannot pause at this early stage; the filename barrier still prevents
    // completion until the handoff decision has been made.
    try { await api.downloads.pause(item.id); pausedByQdm = true; }
    catch (_) { /* filename barrier still keeps this item unfinalized */ }
    const filename = basename(target.filename);
    const headers = await sessionHeaders(config, target.url, target.referrer);
    const accepted = await handoff(withSession({url:target.url,...(filename?{filename}:{})},headers));
    let started = false;
    try {
      const paired = await resolveConnection();
      started = await qdmStarted(paired, accepted.ids && accepted.ids[0]);
    } catch (_) { started = false; }
    if (!started) {
      await feedback('QDM has not confirmed receiving bytes; Dia will continue.', true);
      return false;
    }
    // QDM has proved ownership. Cancel the browser item before releasing its
    // filename determination callback to avoid a second completed file.
    try { await api.downloads.cancel(item.id); }
    catch (error) {
      await feedback('QDM started, but Dia could not cancel its copy: '+String(error.message||error), true);
      return false;
    }
    pausedByQdm = false;
    captured = true;
    try { await api.downloads.erase({id:item.id}); } catch (_) {}
    await feedback('Download transferred to QDM');
    return true;
  } catch (error) {
    try { await feedback('Dia kept the download: '+String(error.message||error), true); } catch (_) {}
    return false;
  } finally {
    if (!captured && pausedByQdm) {
      try { await api.downloads.resume(item.id); } catch (_) {
        try { await feedback('QDM fallback: resume the paused download in Dia.', true); } catch (_) {}
      }
    }
    inflight.delete(item.id);
  }
}

// Chrome/Chromium guarantee that a download cannot finish until all filename
// listeners have invoked suggest(). Always invoke it exactly once, including
// failure, cancellation and unexpected exceptions. Return true for async use.
api.downloads.onDeterminingFilename.addListener((item, suggest) => {
  let called = false;
  const release = () => {
    if (called) return;
    called = true;
    try { suggest(); } catch (_) {}
  };
  if (!eligible(item)) { release(); return; }
  void captureAtFilename(item).then(release,release);
  return true;
});
api.runtime.onInstalled.addListener(async()=>{
  await api.contextMenus.removeAll();
  api.contextMenus.create({id:'qdm-download',title:'Download with QDM',contexts:['link','image','video','audio']});
  const stored=await settings();
  if(typeof stored.autoCapture!=='boolean')await api.storage.local.set({autoCapture:false});
});
api.runtime.onStartup.addListener(()=>{void discoverNative(false).catch(()=>{});});
api.contextMenus.onClicked.addListener(async info=>{
  if(info.menuItemId!=='qdm-download')return;
  try {
    const url=info.linkUrl||info.srcUrl;
    if(!/^https?:\/\//i.test(url||''))throw new Error('HTTP(S) links only');
    const config=await settings();
    const headers=await sessionHeaders(config,url,info.pageUrl||info.frameUrl||'');
    await handoff(withSession({urls:[url]},headers));
    await feedback('Added to QDM');
  } catch(error){await feedback(String(error.message||error),true);}
});
api.runtime.onMessage.addListener((message,_sender,sendResponse)=>{
  if(message?.action!=='qdm-send')return false;
  (async()=>{
    const urls=Array.isArray(message.urls)?message.urls:[];
    const config=await settings();
    let headers={};
    // A single URL may use the referring tab's session. A batch can mix hosts,
    // so forwarding one cookie jar to every URL would leak it across sites.
    if(urls.length===1){
      let referer=typeof message.referrer==='string'?message.referrer:'';
      if(!referer){try{const [tab]=await api.tabs.query({active:true,currentWindow:true});referer=tab?.url||'';}catch{}}
      headers=await sessionHeaders(config,urls[0],referer);
    }
    return handoff(withSession({urls},headers));
  })().then(result=>sendResponse({ok:true,...result}),error=>sendResponse({ok:false,error:String(error.message||error)}));
  return true;
});
