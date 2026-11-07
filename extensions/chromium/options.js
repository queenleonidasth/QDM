'use strict';
const api = chrome;
const token = document.getElementById('token');
const port = document.getElementById('port');
const message = document.getElementById('message');
const capture = document.getElementById('auto-capture');
const session = document.getElementById('forward-session');
api.storage.local.get({token:'',port:17876,autoCapture:false,forwardSession:true}).then(config=>{
  token.value=config.token;port.value=config.port;capture.checked=!!config.autoCapture;session.checked=config.forwardSession!==false;
});
capture.addEventListener('change',async()=>{
  await api.storage.local.set({autoCapture:capture.checked});
  message.textContent=capture.checked?'Automatic capture enabled. Chrome remains fallback.':'Automatic capture disabled.';
});
session.addEventListener('change',async()=>{
  await api.storage.local.set({forwardSession:session.checked});
  message.textContent=session.checked
    ?'Session forwarding enabled. QDM receives cookies, referer and user agent for captured URLs.'
    :'Session forwarding disabled. Protected sessions stay in Chrome.';
});
document.getElementById('connect').addEventListener('click',async()=>{
  try{
    const response=await api.runtime.sendNativeMessage('com.queen.qdm',{action:'discover',start:true});
    if(!response?.ok || !/^[a-f0-9]{64}$/i.test(response.token||'') || !Number.isInteger(response.port)||response.port<17876||response.port>17885)
      throw new Error(response?.error||'QDM native messaging not installed');
    await api.storage.local.set({token:response.token,port:response.port});
    token.value=response.token;port.value=response.port;
    message.textContent='Connected to QDM.';
  }catch(error){message.textContent='Unable to connect: '+String(error.message||error);}
});
document.getElementById('pair-form').addEventListener('submit',async event=>{
  event.preventDefault();
  const config={token:token.value.trim(),port:Number(port.value)};
  if(!/^[a-f0-9]{64}$/i.test(config.token)||!Number.isInteger(config.port)||config.port<17876||config.port>17885){
    message.textContent='Invalid QDM pairing token or port.';return;
  }
  try{
    const response=await fetch('http://127.0.0.1:'+config.port+'/health',{headers:{Authorization:'Bearer '+config.token},signal:AbortSignal.timeout(5000)});
    if(!response.ok||(await response.json()).name!=='QDM')throw new Error('QDM is offline or pairing failed');
    await api.storage.local.set(config);
    message.textContent='Manual pairing saved.';
  }catch(error){message.textContent=String(error.message||error);}
});