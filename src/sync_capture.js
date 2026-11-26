(() => {
  if (window !== top || !window.chrome?.webview) return;
  const post = window.chrome.webview.postMessage.bind(window.chrome.webview);
  const token = __TOKEN__;
  let enabled = false, lastMove = 0;
  Object.defineProperty(window, '__accountdeckSync', {value:Object.freeze({configure(value){enabled=value===true;},get active(){return enabled;}})});
  const sensitive = el => !el || el.closest('input[type=password],[autocomplete^="cc-"],[autocomplete="one-time-code"]');
  function selector(el) {
    if (!(el instanceof Element)) return '';
    const candidates=[];
    for(const attr of ['data-testid','data-test','id','name','aria-label']) {
      const v=el.getAttribute(attr); if(!v) continue;
      const s='['+attr+'='+JSON.stringify(v)+']';
      try{if(document.querySelectorAll(s).length===1)candidates.push(s);}catch{}
    }
    const pieces=[];
    for(let n=el;n&&n.nodeType===1&&pieces.length<12;n=n.parentElement){
      const tag=n.localName;const siblings=n.parentElement?[...n.parentElement.children].filter(x=>x.localName===tag):[n];
      pieces.unshift(tag+':nth-of-type('+(siblings.indexOf(n)+1)+')');
      if(tag==='body')break;
    }
    candidates.push(pieces.join(' > '));
    return JSON.stringify(candidates);
  }
  function send(action){if(enabled)post(JSON.stringify({token,page:location.href,action}));}
  const modifiers=e=>(e.altKey?1:0)|(e.ctrlKey?2:0)|(e.metaKey?4:0)|(e.shiftKey?8:0);
  function point(e){const el=e.target;if(sensitive(el))return null;const r=el.getBoundingClientRect();return {selector:selector(el),rx:r.width?(e.clientX-r.left)/r.width:.5,ry:r.height?(e.clientY-r.top)/r.height:.5};}
  addEventListener('mousemove',e=>{if(!enabled||!e.isTrusted||performance.now()-lastMove<65)return;lastMove=performance.now();const target=point(e);if(target)send({kind:'move',target,modifiers:modifiers(e)});},true);
  addEventListener('click',e=>{if(!enabled||!e.isTrusted||e.button!==0)return;const target=point(e);if(target)send({kind:'click',target,modifiers:modifiers(e),count:Math.max(1,Math.min(2,e.detail))});},true);
  addEventListener('input',e=>{
    if(!enabled||!e.isTrusted||sensitive(e.target))return;
    const el=e.target, editable=el.isContentEditable;
    if(!editable && !(el instanceof HTMLInputElement||el instanceof HTMLTextAreaElement))return;
    if(el instanceof HTMLInputElement && !['text','search','email','url','tel','number','range'].includes(el.type))return;
    const value=editable?el.innerText:el.value;
    if(value.length<=16384)send({kind:'input',selector:selector(el),value,editable});
  },true);
  addEventListener('change',e=>{if(enabled&&e.isTrusted&&!sensitive(e.target)&&e.target instanceof HTMLSelectElement)send({kind:'change',selector:selector(e.target),value:e.target.value});},true);
  addEventListener('keydown',e=>{if(enabled&&e.isTrusted&&!sensitive(e.target)&&['Enter','Tab','Escape','ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Home','End','PageUp','PageDown'].includes(e.key))send({kind:'key',selector:selector(e.target),key:e.key,code:e.code,modifiers:modifiers(e)});},true);
  let scrollTimer;
  addEventListener('scroll',e=>{if(!enabled||!e.isTrusted)return;clearTimeout(scrollTimer);scrollTimer=setTimeout(()=>{const win=e.target===document;const el=win?document.scrollingElement:e.target;if(el)send({kind:'scroll',selector:win?'':selector(el),window:win,x:el.scrollLeft,y:el.scrollTop});},60);},true);
})();
