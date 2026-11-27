((a,expected,passwords)=>{
 if(location.origin+location.pathname!==expected)return {error:'Target is on a different page.'};
 const normalize=s=>(s||'').replace(/\s+/g,' ').trim().slice(0,160);
 function compatible(el,h,strong=false){
   if(!el||!el.isConnected||!el.getClientRects().length)return false;
   if(h.tag&&el.localName!==h.tag)return false;
   if(h.type&&el instanceof HTMLInputElement&&el.type!==h.type)return false;
   if(h.role&&el.getAttribute('role')!==h.role)return false;
   if(strong)return true;
   if(h.aria&&normalize(el.getAttribute('aria-label'))!==h.aria)return false;
   if(h.name&&el.getAttribute('name')!==h.name)return false;
   if(h.href){try{if(new URL(el.href,location.href).pathname!==h.href)return false;}catch{return false;}}
   if(h.text&&normalize(el.innerText||el.textContent)!==h.text)return false;
   return true;
 }
 function find(raw){
   let d;try{d=JSON.parse(raw);}catch{d=[raw];}
   if(Array.isArray(d)){for(const css of d){try{const el=document.querySelector(css);if(el)return el;}catch{}}return null;}
   if(!d||d.version!==2||!Array.isArray(d.candidates))return null;
   for(const c of d.candidates){try{const matches=[...document.querySelectorAll(c.css)].filter(el=>compatible(el,d.hint,!!c.strong));if(matches.length===1)return matches[0];}catch{}}
   const h=d.hint||{};
   if(!(h.aria||h.name||h.href||h.text))return null;
   const tag=/^[a-z][a-z0-9-]*$/i.test(h.tag||'')?h.tag:'*';
   const matches=[...document.querySelectorAll(tag)].filter(el=>compatible(el,h,false));return matches.length===1?matches[0]:null;
 }
 const el=a.kind==='move'||a.kind==='click'?find(a.target.selector):a.selector?find(a.selector):document.documentElement;
 if(!el)return {error:'Matching control was not found or was ambiguous.'};
 if(el.closest('[autocomplete^="cc-" i],[autocomplete="one-time-code" i]')||(!passwords&&el.closest('input[type=password]')))return {error:'Protected field excluded from this sync session.'};
 if(a.kind==='move'||a.kind==='click'){
   if(el.matches(':disabled,[aria-disabled=true]'))return {error:'Target control is disabled.'};
   let r=el.getBoundingClientRect();if(r.width<=0||r.height<=0)return {error:'Target control is hidden.'};
   if(r.bottom<=0||r.top>=innerHeight||r.right<=0||r.left>=innerWidth){el.scrollIntoView({block:'center',inline:'nearest',behavior:'instant'});r=el.getBoundingClientRect();}
   const left=Math.max(1,r.left),right=Math.min(innerWidth-1,r.right),top=Math.max(1,r.top),bottom=Math.min(innerHeight-1,r.bottom);
   if(right<=left||bottom<=top)return {error:'Target control is outside the visible area.'};
   const clamp=(n,lo,hi)=>Math.max(lo,Math.min(hi,n));
   const points=[[clamp(r.left+r.width*a.target.rx,left,right),clamp(r.top+r.height*a.target.ry,top,bottom)],[(left+right)/2,(top+bottom)/2],[left+(right-left)*.25,top+(bottom-top)*.5],[left+(right-left)*.75,top+(bottom-top)*.5]];
   for(const[x,y]of points){const hit=document.elementFromPoint(x,y);if(hit&&(hit===el||el.contains(hit)))return {x,y};}
   return {error:'Another element covers the target control.'};
 }
 if(a.kind==='scroll'){if(a.window)window.scrollTo(a.x,a.y);else el.scrollTo(a.x,a.y);return {done:true};}
 if(a.kind==='change'){if(!(el instanceof HTMLSelectElement))return {error:'Target is not a matching selection field.'};el.value=a.value;el.dispatchEvent(new Event('input',{bubbles:true}));el.dispatchEvent(new Event('change',{bubbles:true}));return {done:true};}
 el.focus({preventScroll:true});
 if(a.kind==='input'){
   if(el.matches(':disabled,[readonly]'))return {error:'Target input is disabled or read-only.'};
   if(a.editable){if(!el.isContentEditable)return {error:'Target is not an editable field.'};const range=document.createRange();range.selectNodeContents(el);const selection=getSelection();selection.removeAllRanges();selection.addRange(range);}
   else{
     if(!(el instanceof HTMLInputElement||el instanceof HTMLTextAreaElement))return {error:'Target is not an input field.'};
     if(el instanceof HTMLInputElement&&!['text','search','email','url','tel','number','range','password'].includes(el.type))return {error:'Unsupported target input type.'};
     if(el instanceof HTMLInputElement&&el.type==='range'){Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(el,a.value);el.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertReplacementText',data:a.value}));return {done:true};}
     try{el.setSelectionRange(0,el.value.length);}catch{return {ready:true,select_all:true};}
   }
 }
 return {ready:true};
})
