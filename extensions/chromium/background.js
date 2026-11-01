'use strict';
const api = globalThis.chrome;
const nativeHost = 'com.queen.qdm';
const defaults = { token: '', port: 17876, autoCapture: false };
const inflight = new Set();
let pairing = null;
const valid = cfg => /^[a-f0-9]{64}$/i.test(cfg?.token || '') && Number.isInteger(cfg?.port) && cfg.port >= 17876 && cfg.port <= 17885;
async function settings() { return api.storage.local.get(defaults); }
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
async function capture(item) {
  if(!eligible(item)||inflight.has(item.id))return;
  inflight.add(item.id);
  try {
    if(!(await settings()).autoCapture)return;
    // Cancel the browser only after QDM explicitly acknowledges the request.
    // A failed, paused, or protected handoff leaves Chrome's download alone.
    const filename=basename(item.filename);
    await handoff({url:item.url,...(filename?{filename}:{})});
    await api.downloads.cancel(item.id);
    try{await api.downloads.erase({id:item.id});}catch{}
    await feedback('Download accepted by QDM');
  } catch(error) {
    await feedback('Chrome kept the original download: '+String(error.message||error),true);
  } finally {inflight.delete(item.id);}
}
api.downloads.onCreated.addListener(item=>{void capture(item);});
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
    await handoff({urls:[url]});
    await feedback('Added to QDM');
  } catch(error){await feedback(String(error.message||error),true);}
});
api.runtime.onMessage.addListener((message,_sender,sendResponse)=>{
  if(message?.action!=='qdm-send')return false;
  handoff({urls:message.urls}).then(result=>sendResponse({ok:true,...result}),error=>sendResponse({ok:false,error:String(error.message||error)}));
  return true;
});
