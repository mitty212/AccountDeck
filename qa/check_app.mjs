// Developer-owned release-binary integration check. Uses an isolated test workspace.
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {createServer as tcpServer} from 'node:net';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const output=path.join(root,'qa','output','app-'+Date.now());await mkdir(output,{recursive:true});
const listener=tcpServer();await new Promise(r=>listener.listen(0,'127.0.0.1',r));const debugPort=listener.address().port;await new Promise(r=>listener.close(r));
const fixture=createServer((req,res)=>{res.writeHead(200,{'Content-Type':'text/html'});res.end('<!doctype html><title>Local account test</title><body style="font:24px Segoe UI;padding:40px">Local account fixture<script>localStorage.setItem("visited","yes")</script>');});
await new Promise(r=>fixture.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+fixture.address().port;
const child=spawn(path.join(root,'target','release','accountdeck.exe'),['--data-dir',path.join(output,'workspace')],{windowsHide:true,env:{...process.env,WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:'--remote-debugging-address=127.0.0.1 --remote-debugging-port='+debugPort},stdio:'ignore'});
const delay=ms=>new Promise(r=>setTimeout(r,ms));
let ws;
try{
 let target;for(let i=0;i<100;i++){try{const list=await (await fetch('http://127.0.0.1:'+debugPort+'/json/list')).json();target=list.find(t=>t.type==='page'&&t.title==='AccountDeck');if(target)break;}catch{}await delay(100);}
 assert(target,'Release shell did not appear in its test debugger');
 ws=new WebSocket(target.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j;});
 let seq=0;const pending=new Map();ws.onmessage=e=>{const m=JSON.parse(e.data);if(m.id&&pending.has(m.id)){const {r,j}=pending.get(m.id);pending.delete(m.id);m.error?j(Error(m.error.message)):r(m.result);}};
 const call=(method,params={})=>new Promise((r,j)=>{const id=++seq;pending.set(id,{r,j});ws.send(JSON.stringify({id,method,params}));setTimeout(()=>{if(pending.delete(id))j(Error('Debugger command timeout: '+method));},10000).unref();});
 const evaluate=async expression=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.text);return r.result.value;};
 const wait=async expression=>{for(let i=0;i<100;i++){if(await evaluate(expression))return;await delay(100);}throw Error('State condition timed out: '+expression);};
 await wait('typeof window.receiveState === "function" && state.version === "0.1.0"');
 await call('Page.enable');
 let shot=await call('Page.captureScreenshot',{format:'png'});await writeFile(path.join(output,'native-empty.png'),Buffer.from(shot.data,'base64'));
 await evaluate('document.getElementById("addAccount").click()');
 assert(await evaluate('!document.getElementById("modalBackdrop").classList.contains("hidden")'));
 await evaluate(`{const inputs=document.querySelectorAll('#modal input');inputs[0].value='Local A';inputs[1].value=${JSON.stringify(origin)};[...document.querySelectorAll('#modal button')].find(b=>b.textContent==='Create account').click();}`);
 await wait('state.accounts.length===1 && state.loaded.length===1');
 const id=await evaluate('state.accounts[0].id');
 await evaluate(`send({op:'duplicate',id:${JSON.stringify(id)}})`);await wait('state.accounts.length===2 && state.loaded.length===2');
 const copy=await evaluate('state.active');assert.notEqual(id,copy);
 await evaluate(`send({op:'mute',id:${JSON.stringify(copy)}});send({op:'pin',id:${JSON.stringify(copy)}});send({op:'zoom',value:1.2});`);
 await wait('state.accounts[1].muted && state.accounts[1].pinned && state.accounts[1].zoom===1.2');
 await evaluate(`send({op:'sleep',id:${JSON.stringify(copy)}})`);await wait('state.loaded.length===1');
 await evaluate(`send({op:'switch',id:${JSON.stringify(copy)}})`);await wait('state.loaded.length===2 && state.active==='+JSON.stringify(copy));
 await evaluate(`send({op:'password',value:'local-fixture-password'});send({op:'lock'});`);await wait('state.locked===true');
 await evaluate(`send({op:'unlock',value:'local-fixture-password'})`);await wait('state.locked===false');
 await evaluate(`send({op:'password',value:''});send({op:'overlay',visible:true});document.getElementById('settings').click();`);
 await wait('!document.getElementById("modalBackdrop").classList.contains("hidden")');
 shot=await call('Page.captureScreenshot',{format:'png'});await writeFile(path.join(output,'native-settings.png'),Buffer.from(shot.data,'base64'));
 const config=JSON.parse(await readFile(path.join(output,'workspace','accounts.json'),'utf8'));assert.equal(config.accounts.length,2);assert.equal(config.accounts[1].muted,true);assert.equal(config.password_hash,null);
 const result={passed:true,checks:['release-native-shell-loaded','DOM-add-account-through-native-IPC','independent-duplicate-profile','mute-pin-zoom','sleep-and-resume','workspace-lock-and-unlock','settings-modal','atomic-config-persisted'],output};await writeFile(path.join(output,'summary.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
}finally{ws?.close();child.kill();fixture.close();}
