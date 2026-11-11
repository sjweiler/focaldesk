// Rebuild pasted HTML into a detached allowlisted tree before it enters the editor.
export function cleanHtml(source:string):string {
 const parsed=document.createElement('template');parsed.innerHTML=source;
 const output=document.createElement('div');
 const allowed=new Set(['P','DIV','BR','STRONG','B','EM','I','U','UL','OL','LI','A','BLOCKQUOTE']);
 const dropped=new Set(['SCRIPT','STYLE','IFRAME','OBJECT','EMBED','SVG','MATH','TEMPLATE','IMG','VIDEO','AUDIO']);
 function copy(node:Node,parent:HTMLElement,depth:number){
  if(depth>64)return;
  if(node.nodeType===Node.TEXT_NODE){parent.append(document.createTextNode(node.textContent||''));return;}
  if(!(node instanceof Element)||dropped.has(node.tagName))return;
  const target=allowed.has(node.tagName)?document.createElement(node.tagName.toLowerCase()):parent;
  if(target!==parent){
   if(node.tagName==='A') {const href=node.getAttribute('href')||'';try{const url=new URL(href);if(['https:','http:'].includes(url.protocol)&&!url.username&&!url.password)target.setAttribute('href',url.href);}catch{}}
   if(node.tagName==='DIV'&&node.getAttribute('data-focal-signature')==='true')target.dataset.focalSignature='true';
   parent.append(target);
  }
  for(const child of node.childNodes)copy(child,target,depth+1);
 }
 for(const child of parsed.content.childNodes)copy(child,output,0);
 return output.innerHTML;
}
export function textHtml(text:string):string {
 const div=document.createElement('div');div.textContent=text;
 return div.innerHTML.replace(/\r?\n/g,'<br>');
}
export function plainText(editor:HTMLElement):string {return editor.innerText.replace(/\u00a0/g,' ');}

export function setupEditor(editor:HTMLElement,toolbar:HTMLElement,onChange:()=>void){
 const commands=[['Bold','bold'],['Italic','italic'],['Bulleted list','insertUnorderedList'],['Numbered list','insertOrderedList'],['Undo','undo'],['Redo','redo']];
 const run=(command:string,value?:string)=>{editor.focus();document.execCommand(command,false,value);onChange();};
 for(const [label,command] of commands){
  const button=document.createElement('button');button.type='button';button.textContent=label;
  button.addEventListener('mousedown',e=>e.preventDefault());button.onclick=()=>run(command);toolbar.append(button);
 }
 const urlInput=document.createElement('input');urlInput.type='text';urlInput.inputMode='url';urlInput.placeholder='https://…';urlInput.setAttribute('aria-label','Link URL');
 let linkRange:Range|null=null;
 const rememberSelection=()=>{
  const selection=window.getSelection();
  if(selection?.rangeCount&&editor.contains(selection.anchorNode))linkRange=selection.getRangeAt(0).cloneRange();
 };
 // Capture before focus moves to the URL field, including keyboard navigation.
 toolbar.addEventListener('mousedown',rememberSelection);
 editor.addEventListener('keydown',e=>{if(e.key==='Tab')rememberSelection();});
 editor.addEventListener('keyup',rememberSelection);
 editor.addEventListener('mouseup',rememberSelection);
 urlInput.addEventListener('focus',rememberSelection);
 const linkStatus=document.createElement('span');linkStatus.setAttribute('role','status');
 const link=document.createElement('button');link.type='button';link.textContent='Add link';
 link.addEventListener('mousedown',e=>e.preventDefault());
 link.onclick=()=>{
  try{
   const url=new URL(urlInput.value);
   if(!['https:','http:'].includes(url.protocol)||url.username||url.password)throw new Error('Invalid URL');
   linkStatus.textContent='';editor.focus();
   if(linkRange&&editor.contains(linkRange.commonAncestorContainer)){const selection=window.getSelection();selection?.removeAllRanges();selection?.addRange(linkRange);}
   run('createLink',url.href);urlInput.value='';linkRange=null;
  }catch{linkStatus.textContent='Enter an HTTP or HTTPS URL without sign-in details.';}
 };
 urlInput.addEventListener('input',()=>{linkStatus.textContent='';});
 toolbar.append(urlInput,link,linkStatus);
 editor.addEventListener('click',e=>{if((e.target as Element).closest('a'))e.preventDefault();});
 editor.addEventListener('paste',e=>{
  e.preventDefault();const html=e.clipboardData?.getData('text/html');
  document.execCommand('insertHTML',false,html?cleanHtml(html):textHtml(e.clipboardData?.getData('text/plain')||''));onChange();
 });
 // Prevent dropped files or HTML from bypassing the paste sanitizer.
 editor.addEventListener('drop',e=>e.preventDefault());
 editor.addEventListener('input',onChange);
}
