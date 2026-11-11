import {decodeChats,decodeProjects,initialProjects,type ConsoleState,type Chat,type Project} from './conversations.ts';
import {normalizeSettings,defaultSettings} from './settings.ts';
export function decodeState(value:unknown):ConsoleState{
 if(Array.isArray(value))return {version:2,chats:decodeChats(JSON.stringify(value)),projects:ensureProjects(initialProjects,value as Chat[]),settings:{...defaultSettings}};
 if(!value||typeof value!=='object')throw new Error('Invalid console state.');
 const v=value as Record<string,unknown>;const chats=decodeChats(JSON.stringify(v.chats));
 return {version:2,chats,projects:ensureProjects(decodeProjects(v.projects),chats),settings:normalizeSettings(v.settings)};
}
export function ensureProjects(projects:Project[],chats:Chat[]):Project[]{
 const result=projects.map(p=>({...p,sources:p.sources&&[...p.sources]}));
 for(const chat of chats){if(!chat.project.trim()||chat.project.length>80)chat.project='General';if(!result.some(p=>p.name===chat.project)){if(result.some(p=>p.name.toLowerCase()===chat.project.toLowerCase())){chat.project=result.find(p=>p.name.toLowerCase()===chat.project.toLowerCase())!.name;}else result.push({name:chat.project,sources:null});}}
 return result;
}
