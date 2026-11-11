import type {Message,GenerationSettings} from './services';
export type Chat={id:number;title:string;messages:Message[];project:string;example?:boolean};
export type Project={name:string;sources:string[]|null};
export type ConsoleState={version:2;chats:Chat[];projects:Project[];settings:GenerationSettings};
export const storageKey='focaldesk.console.conversations.v1';
export const initialProjects:Project[]=['General','Development','Research','MSAIE','FocalDesk','Voice / PBX'].map(name=>({name,sources:null}));
export function decodeChats(raw:string):Chat[]{
 const value:unknown=JSON.parse(raw);
 if(!Array.isArray(value)||!value.every(c=>c&&Number.isSafeInteger(c.id)&&typeof c.title==='string'&&typeof c.project==='string'&&Array.isArray(c.messages)&&c.messages.every((m:Message)=>m&&['user','assistant'].includes(m.role)&&typeof m.content==='string'&&(m.attachments===undefined||(Array.isArray(m.attachments)&&m.attachments.every(a=>typeof a.name==='string'&&typeof a.content==='string'&&(a.data_url===undefined||typeof a.data_url==='string'&&/^data:image\/(png|jpeg|webp);base64,/.test(a.data_url)))))&&(m.citations===undefined||Array.isArray(m.citations)&&m.citations.every(a=>typeof a.memory_id==='number'&&typeof a.excerpt==='string'&&(a.source===undefined||typeof a.source==='string'))))))throw new Error('Saved conversation format is invalid.');
 return value;
}
export function decodeProjects(value:unknown):Project[]{
 if(!Array.isArray(value)||!value.every(p=>p&&typeof p.name==='string'&&p.name.trim()&&p.name.length<=80&&(p.sources===null||Array.isArray(p.sources)&&p.sources.every((s:unknown)=>typeof s==='string'))))throw new Error('Invalid workspace definitions.');
 if(new Set(value.map(p=>p.name.toLowerCase())).size!==value.length)throw new Error('Workspace names must be unique.');
 return value.some(p=>p.name==='General')?value:[{name:'General',sources:null},...value];
}
export function appendReply(chats:Chat[],id:number,messages:Message[]):Chat[]{return chats.map(chat=>chat.id===id?{...chat,messages,title:chat.messages.length===0?messages[0]?.content.slice(0,48)||'New chat':chat.title}:chat);}
export function importChats(current:Chat[],incoming:Chat[]):Chat[]{
 const used=new Set(current.map(c=>c.id));let next=Math.max(Date.now(),...used)+1;
 return [...incoming.map(c=>{let id=c.id;if(used.has(id))id=next++;used.add(id);return {...c,id};}),...current];
}
export function convertLegacy(value:unknown):Chat[]{
 if(!value||typeof value!=='object'||!Array.isArray((value as {conversations?:unknown}).conversations))throw new Error('Not a legacy AI Console history file.');
 return (value as {conversations:{title:string;messages:string[];summary?:string}[]}).conversations.map((c,i)=>{
  if(typeof c.title!=='string'||!Array.isArray(c.messages)||!c.messages.every(m=>typeof m==='string'))throw new Error('Invalid legacy conversation.');
  const messages:Message[]=c.messages.flatMap<Message>(content=>content.startsWith('User: ')?[{role:'user' as const,content:content.slice(6)}]:content.startsWith('AI: ')?[{role:'assistant' as const,content:content.slice(4)}]:[]);
  return {id:Date.now()+i,title:c.title,project:'General',messages};
 });
}
export function renameProject(chats:Chat[],from:string,to:string):Chat[]{return chats.map(c=>c.project===from?{...c,project:to}:c);}
