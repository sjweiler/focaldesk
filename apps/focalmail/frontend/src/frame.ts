import bootstrap from './mail-frame.js?raw';
import type {Message} from './types';
// This hash is also in tauri.conf.json; the browser test verifies both against the file.
export const FRAME_SCRIPT_HASH='sha256-1iWAf0AaQHCiUIX/Z4K4QX2MYm2+ZnasW8xDxBWVV/I=';
const css=`html{color-scheme:light}body{box-sizing:border-box;margin:0;padding:16px;font:15px/1.65 system-ui,sans-serif;background:#fff;color:#20252c;overflow-wrap:anywhere}*{box-sizing:border-box}img{max-width:100%;height:auto}img:not([src]){display:inline-block;min-width:100px;min-height:32px;padding:6px;border:1px dashed #a6aeb9;color:#6a7381}table{max-width:100%;border-collapse:collapse}pre{white-space:pre-wrap}a{color:#146fd0}blockquote{margin-left:0;padding-left:16px;border-left:3px solid #c2cbd7}button,input,form,iframe,object,embed,video,audio{display:none!important}`;
export function createMailFrame(m:Message,remote:Map<number,string>,openLink:(url:string)=>void){
 const doc=m.html!;
 let layout=doc.layout!;
 for(const image of doc.images||[]){
   const data=image.inline_data||remote.get(image.index);
   if(data && /^data:image\/(png|jpeg|gif|webp);base64,[a-zA-Z0-9+/=]+$/.test(data))layout=layout.replace(` data-mail-image="${image.index}"`,` data-mail-image="${image.index}" src="${data}"`);
 }
 const csp=`default-src 'none'; script-src '${FRAME_SCRIPT_HASH}'; style-src 'unsafe-inline'; img-src data:; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'`;
 const frame=document.createElement('iframe');frame.className='mail-frame';frame.title=`Formatted email: ${m.subject||'No subject'}`;
 frame.sandbox.add('allow-scripts');frame.referrerPolicy='no-referrer';frame.setAttribute('allow',"camera 'none'; microphone 'none'; geolocation 'none'");
 frame.srcdoc=`<!doctype html><html><head><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="${csp}"><meta name="referrer" content="no-referrer"><style>${css}</style></head><body>${layout}<script>${bootstrap}</script></body></html>`;
 const listener=(e:MessageEvent)=>{
   if(e.source!==frame.contentWindow||!e.data||typeof e.data!=='object')return;
   if(e.data.type==='mail-size' && typeof e.data.height==='number' && Number.isFinite(e.data.height))frame.style.height=`${Math.min(20000,Math.max(220,e.data.height))}px`;
   if(e.data.type==='mail-link'&&typeof e.data.url==='string'){
     try{const u=new URL(e.data.url);if(['http:','https:'].includes(u.protocol)&&!u.username&&!u.password)openLink(u.href);}catch{/* Inert link. */}
   }
 };
 window.addEventListener('message',listener);
 return {frame,dispose:()=>window.removeEventListener('message',listener)};
}
