// Optional network smoke test with fresh profiles and dummy values only. Never submits login/recovery forms.
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir,writeFile,readFile,readdir} from 'node:fs/promises';
import {once} from 'node:events';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'..');
const output=path.join(root,'qa/output','facebook-'+Date.now());await mkdir(output,{recursive:true});
const workspace=path.join(output,'workspace');
const delay=ms=>new Promise(r=>setTimeout(r,ms));const base=20000+Math.floor(Math.random()*9000);
const port=id=>base+(Number.parseInt(id.slice(0,8),16)%10000)+1;
const connections=[];let child;let observed=[];
async function connect(port,match){for(let i=0;i<150;i++){try{const list=await(await fetch('http://127.0.0.1:'+port+'/json/list')).json();const t=list.find(match);if(t){const ws=new WebSocket(t.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j;});let seq=0;const pending=new Map();ws.onmessage=e=>{const m=JSON.parse(e.data);const p=pending.get(m.id);if(p){pending.delete(m.id);m.error?p.j(Error(m.error.message)):p.r(m.result);}};const call=(method,params={})=>new Promise((r,j)=>{const id=++seq;pending.set(id,{r,j});ws.send(JSON.stringify({id,method,params}));setTimeout(()=>{if(pending.delete(id))j(Error('CDP timeout '+method));},15000).unref();});const evalJS=async expression=>{const r=await call('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true,userGesture:true});if(r.exceptionDetails)throw Error(r.exceptionDetails.exception?.description||r.exceptionDetails.text);return r.result.value;};const c={call,eval:evalJS,ws};connections.push(c);return c;}}catch{}await delay(100);}throw Error('Missing debugger page on port '+port);}
async function wait(c,expression){const deadline=Date.now()+12000;for(let i=0;i<120&&Date.now()<deadline;i++){try{if(await c.eval(expression))return;}catch{}await delay(80);}throw Error('Condition timeout: '+expression);}
async function mouse(c,selector,click=false){const p=await c.eval(`(()=>{const r=document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();return{x:r.left+r.width/2,y:r.top+r.height/2}})()`);await c.call('Input.dispatchMouseEvent',{type:'mouseMoved',...p});if(click){await c.call('Input.dispatchMouseEvent',{type:'mousePressed',...p,button:'left',clickCount:1});await c.call('Input.dispatchMouseEvent',{type:'mouseReleased',...p,button:'left',clickCount:1});}}
async function launch(){const env={...process.env,ACCOUNTDECK_QA_CDP_BASE:String(base)};delete env.WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS;child=spawn(path.join(root,'target/release/accountdeck.exe'),['--data-dir',workspace],{windowsHide:true,env,stdio:'ignore'});return connect(base,t=>t.title==='AccountDeck');}
const checks=[];
try {
 const shell=await launch();await wait(shell,'state.version==="0.3.0"');const ids=[];
 for(const name of ['Source','Target']){await shell.eval(`send({op:'add',name:'Facebook QA ${name}',url:'https://www.facebook.com/',group:'QA'})`);await wait(shell,'state.accounts.length==='+ (ids.length+1));ids.push(await shell.eval('state.active'));}
 const pages=[];observed=pages;for(const id of ids){const c=await connect(port(id),t=>t.url.includes('facebook.com'));pages.push(c);}
 for(const c of pages)await wait(c,'document.readyState==="complete"&&document.querySelector("input[name=email]")!==null&&document.querySelector("input[name=pass]")!==null');await delay(1500);
 await shell.eval(`send({op:'switch',id:${JSON.stringify(ids[0])}})`);await wait(shell,'state.active==='+JSON.stringify(ids[0]));
 const target=await shell.eval(`state.sync_candidates.find(t=>t.profile===${JSON.stringify(ids[1])}).id`);await shell.eval(`send({op:'sync_start',targets:[${JSON.stringify(target)}],include_passwords:true})`);await wait(shell,'state.sync_ready===true');await wait(pages[1],'document.visibilityState==="visible"');
 await wait(pages[0],'document.querySelector("input[name=email]")!==null');await mouse(pages[0],'input[name=email]',true);await pages[0].call('Input.insertText',{text:'accountdeck-sync-check@example.invalid'});await wait(pages[1],'document.querySelector("input[name=email]").value==="accountdeck-sync-check@example.invalid"');checks.push('live-facebook-email-input');
 await mouse(pages[0],'input[name=pass]',true);await pages[0].call('Input.insertText',{text:'AccountDeck-fixture-only-123!'});await wait(pages[1],'document.querySelector("input[name=pass]").value==="AccountDeck-fixture-only-123!"');checks.push('live-facebook-password-opt-in');
 // Clear dummy values before following the recovery link; no form is submitted.
 for(const c of pages)await c.eval('for(const name of ["email","pass"])document.querySelector("input[name="+name+"]").value=""');
 const link=await pages[0].eval('[...document.querySelectorAll("a")].find(e=>/forgot|quên/i.test(e.textContent))?.getAttribute("href")');assert(link,'Public login page did not expose a recovery link');
 await mouse(pages[0],'a[href='+JSON.stringify(link)+']',true);for(const c of pages)await wait(c,'/identify|recover/.test(location.pathname)');checks.push('live-facebook-forgotten-password-link-both-profiles');
 const paths=await Promise.all(pages.map(c=>c.eval('location.pathname')));await shell.eval('send({op:"sync_stop"})');
 const exit=once(child,'exit');await shell.eval('send({op:"quit"})');await exit;
 const result={passed:true,checks,paths,output};await writeFile(path.join(output,'summary.json'),JSON.stringify(result,null,2));console.log(JSON.stringify(result));
} catch(error) {
 const diagnostics=[];for(const c of observed){try{diagnostics.push(await c.eval('({path:location.pathname,title:document.title,inputNames:[...document.querySelectorAll("input")].map(e=>e.name),notice:document.body.innerText.slice(0,160)})'))}catch{}}
 await writeFile(path.join(output,'summary.json'),JSON.stringify({passed:false,checks,error:error.message,diagnostics},null,2));throw error;
} finally {for(const c of connections)c.ws.close();if(child&&child.exitCode===null)child.kill();}
