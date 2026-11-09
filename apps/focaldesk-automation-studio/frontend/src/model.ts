import type { Node, Edge } from '@xyflow/svelte';
export type Kind = 'schedule'|'condition'|'notify'|'log'|'volume'|'wifi'|'bluetooth'|'ai_decision'|'webhook';
export type Block = {kind:Kind; schedule?:string; hour?:number; title?:string; body?:string; message?:string; level?:number; enabled?:boolean; prompt?:string; path?:string};
export type FlowNode = Node<Block, 'block'>;
export type Workflow = {version:1; id:string; name:string; nodes:FlowNode[]; edges:Edge[]};
export const catalog: {kind:Kind; label:string; icon:string; color:string; group:string; description:string; draft?:boolean}[] = [
  {kind:'schedule',label:'Schedule',icon:'◷',color:'green',group:'Triggers',description:'Run daily or at an interval'},
  {kind:'webhook',label:'Webhook',icon:'⌘',color:'pink',group:'Triggers',description:'Incoming event · draft only',draft:true},
  {kind:'condition',label:'Time condition',icon:'◇',color:'blue',group:'Logic & intelligence',description:'Branch by local time'},
  {kind:'ai_decision',label:'AI decision',icon:'✧',color:'purple',group:'Logic & intelligence',description:'Model decision · draft only',draft:true},
  {kind:'notify',label:'Notification',icon:'▣',color:'blue',group:'Desktop actions',description:'Send a desktop notification'},
  {kind:'log',label:'Write to log',icon:'≡',color:'yellow',group:'Desktop actions',description:'Record a message in the journal'},
  {kind:'volume',label:'Set volume',icon:'◖',color:'purple',group:'Desktop actions',description:'Adjust the desktop volume'},
  {kind:'wifi',label:'Wi-Fi',icon:'⌁',color:'green',group:'Desktop actions',description:'Turn wireless networking on or off'},
  {kind:'bluetooth',label:'Bluetooth',icon:'ᛒ',color:'blue',group:'Desktop actions',description:'Turn Bluetooth on or off'},
];
export const defaults: Record<Kind, Block> = {
  schedule:{kind:'schedule',schedule:'daily 09:00'},condition:{kind:'condition',hour:9},
  notify:{kind:'notify',title:'Good morning',body:'Your workspace is ready.'},log:{kind:'log',message:'Workflow completed'},
  volume:{kind:'volume',level:35},wifi:{kind:'wifi',enabled:true},bluetooth:{kind:'bluetooth',enabled:true},
  ai_decision:{kind:'ai_decision',prompt:'Should I enable focus mode?'},webhook:{kind:'webhook',path:'/events'},
};
export function subtitle(d: Block): string {
  switch(d.kind) {
    case 'schedule':return d.schedule || 'Set a schedule';
    case 'condition':return `Local hour ≥ ${d.hour}:00`;
    case 'notify':return d.title || 'Untitled notification';
    case 'log':return d.message || 'Write to journal';
    case 'volume':return `Set volume to ${d.level}%`;
    case 'wifi':case 'bluetooth':return d.enabled ? 'Turn on' : 'Turn off';
    default:return 'Draft only · not connected to engine';
  }
}
export function starter(): Workflow {
  const node = (id:string,kind:Kind,x:number,y:number):FlowNode => ({id,type:'block',position:{x,y},data:{...defaults[kind]}});
  return {version:1,id:crypto.randomUUID(),name:'Morning workspace',nodes:[
    node('schedule','schedule',260,30),node('condition','condition',260,190),
    {...node('notify','notify',70,370),selected:true},node('volume','volume',470,370),node('log','log',70,540),
  ],edges:[
    {id:'a',source:'schedule',target:'condition',type:'smoothstep'},
    {id:'b',source:'condition',sourceHandle:'yes',target:'notify',label:'Yes',type:'smoothstep'},
    {id:'c',source:'condition',sourceHandle:'no',target:'volume',label:'No',type:'smoothstep'},
    {id:'d',source:'notify',target:'log',type:'smoothstep'},
  ]};
}
