(() => {
  if (window !== top || !window.chrome?.webview) return;
  const post=window.chrome.webview.postMessage.bind(window.chrome.webview),token=__TOKEN__;
  let enabled=false,includePasswords=false,lastMove=0,pointerClick=false,lastClick={time:0,key:''};
  const emit=(action,page=location.href)=>post(JSON.stringify({token,page,action}));
  Object.defineProperty(window,'__accountdeckSync',{value:Object.freeze({
    configure(value,passwords=false){const active=value===true,allow=passwords===true;if(active!==enabled||allow!==includePasswords){enabled=active;includePasswords=allow;emit({kind:'ready',enabled});}},
    get active(){return enabled;}
  })});
  const protectedSelector='[autocomplete^="cc-" i],[autocomplete="one-time-code" i]';
  const sensitive=el=>!el||!(el instanceof Element)||el.closest(protectedSelector)||(!includePasswords&&el.closest('input[type=password]'));
  const normalize=s=>(s||'').replace(/\s+/g,' ').trim().slice(0,160);
  const interactive='a[href],button,input,select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=menuitem],[role=tab],[contenteditable=true],[onclick]';
  const control=el=>el instanceof Element?(el.closest(interactive)||el):null;
  function descriptor(el){
    if(!(el instanceof Element))return '';
    const hint={tag:el.localName,role:el.getAttribute('role')||'',type:el instanceof HTMLInputElement?el.type:'',aria:normalize(el.getAttribute('aria-label')),name:el.getAttribute('name')||'',text:''};
    if(!hint.aria&&!['input','textarea','select'].includes(hint.tag)&&!el.isContentEditable)hint.text=normalize(el.innerText||el.textContent);
    if(el instanceof HTMLAnchorElement){try{hint.href=new URL(el.href,location.href).pathname}catch{}}
    const candidates=[];
    for(const attr of ['data-testid','data-test','name','aria-label','id']){
      const value=el.getAttribute(attr);if(!value||value.length>180)continue;
      const css=el.localName+'['+attr+'='+JSON.stringify(value)+']';
      try{if(document.querySelectorAll(css).length===1)candidates.push({css,strong:attr==='data-testid'||attr==='data-test'||attr==='name'||attr==='aria-label'});}catch{}
    }
    const pieces=[];for(let n=el;n&&n.nodeType===1&&pieces.length<12;n=n.parentElement){const tag=n.localName;const siblings=n.parentElement?[...n.parentElement.children].filter(x=>x.localName===tag):[n];pieces.unshift(tag+':nth-of-type('+(siblings.indexOf(n)+1)+')');if(tag==='body')break;}
    candidates.push({css:pieces.join(' > '),strong:false});return JSON.stringify({version:2,hint,candidates});
  }
  const modifiers=e=>(e.altKey?1:0)|(e.ctrlKey?2:0)|(e.metaKey?4:0)|(e.shiftKey?8:0);
  function point(e,node=control(e.target)){
    if(sensitive(node))return null;const r=node.getBoundingClientRect();
    return {selector:descriptor(node),rx:Math.max(0,Math.min(1,r.width?(e.clientX-r.left)/r.width:.5)),ry:Math.max(0,Math.min(1,r.height?(e.clientY-r.top)/r.height:.5))};
  }
  function click(e,target,page=location.href){if(!target)return;const time=performance.now(),key=target.selector;const count=time-lastClick.time<500&&key===lastClick.key?2:1;lastClick={time,key};emit({kind:'click',target,modifiers:modifiers(e),count},page);}
  addEventListener('mousemove',e=>{if(!enabled||!e.isTrusted||performance.now()-lastMove<65)return;lastMove=performance.now();const target=point(e);if(target)emit({kind:'move',target,modifiers:modifiers(e)});},true);
  // Send on the initial trusted press: an immediate navigation can discard a message posted on release.
  // Sync mirrors clicks rather than drag gestures; resolve the control before site handlers mutate it.
  addEventListener('pointerdown',e=>{if(!enabled||!e.isTrusted||e.button!==0)return;pointerClick=true;click(e,point(e));},true);
  addEventListener('click',e=>{if(!enabled||!e.isTrusted||e.button!==0)return;if(pointerClick){pointerClick=false;return}if(e.detail!==0)click(e,point(e));},true);
  addEventListener('input',e=>{
    if(!enabled||!e.isTrusted||sensitive(e.target))return;
    const el=e.target,editable=el.isContentEditable;
    if(!editable&&!(el instanceof HTMLInputElement||el instanceof HTMLTextAreaElement))return;
    if(el instanceof HTMLInputElement&&!['text','search','email','url','tel','number','range','password'].includes(el.type))return;
    const value=editable?el.innerText:el.value;if(value.length<=16384)emit({kind:'input',selector:descriptor(el),value,editable});
  },true);
  addEventListener('change',e=>{if(enabled&&e.isTrusted&&!sensitive(e.target)&&e.target instanceof HTMLSelectElement)emit({kind:'change',selector:descriptor(e.target),value:e.target.value});},true);
  addEventListener('keydown',e=>{if(enabled&&e.isTrusted&&!sensitive(e.target)&&['Enter','Tab','Escape','ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Home','End','PageUp','PageDown'].includes(e.key))emit({kind:'key',selector:descriptor(e.target),key:e.key,code:e.code,modifiers:modifiers(e)});},true);
  let scrollTimer;addEventListener('scroll',e=>{if(!enabled||!e.isTrusted)return;clearTimeout(scrollTimer);scrollTimer=setTimeout(()=>{const win=e.target===document,el=win?document.scrollingElement:e.target;if(el)emit({kind:'scroll',selector:win?'':descriptor(el),window:win,x:el.scrollLeft,y:el.scrollTop});},60);},true);
})();
