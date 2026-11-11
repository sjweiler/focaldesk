import type {GenerationSettings} from './settings';
export {defaultSettings} from './settings';
export type {GenerationSettings} from './settings';
import {invoke, isTauri, Channel} from '@tauri-apps/api/core';
export type Citation = {memory_id:number; source?:string; excerpt:string; distance:number};
export type Attachment = {name:string;content:string;data_url?:string;media_type?:string;truncated?:boolean};
export type Message = {role:'user'|'assistant'; content:string; citations?:Citation[]; attachments?:Attachment[];cancelled?:boolean};
export type Provider = {id:string; kind:string; default_model?:string};
export type ChatResponse = {content:string;citations?:Citation[];provider:string;model?:string;usage?:{input_tokens:number;output_tokens:number}};
function requireDesktop() {
  if (!isTauri()) throw new Error('Open the Tauri desktop app to connect to the FocalDesk AI daemon.');
}
export async function listProviders() { requireDesktop(); return invoke<Provider[]>('list_providers'); }
export async function listModels(provider:string) { requireDesktop(); return invoke<{id:string}[]>('list_models', {provider}); }
export async function sendMessage(messages:Message[], provider:string, useMemory:boolean, model:string) {
  requireDesktop();
  return invoke<ChatResponse>('send_message', {messages:messages.map(({role,content,attachments})=>({role,content:content+(attachments||[]).map(a=>`\n\n--- Attached document: ${a.name} ---\n${a.content}\n--- End document ---`).join('')})), provider:provider || null, useMemory, model:model || null});
}
export type DaemonStatus = {active_requests:number;pending_permissions:number;default_provider:string;provider_count:number;provider_telemetry:{provider:string;requests:number;successes:number;failures:number;last_latency_ms?:number;input_tokens:number;output_tokens:number;last_error?:string}[]};
export type MemoryStatus = {entry_count:number;vector_backend:string};
export type Agent = {id:string;name:string;description:string;tool_allowlist:string[]};
export type AgentRun = {run_id:string;agent_id:string;state:string;objective_preview:string;completed_tool_steps:number;error?:string;result?:{answer?:string;confirmation?:{plan_id:string;tool:string};proposed_action?:{tool:string;arguments:unknown}};observations:unknown[]};
export type Connector = {manifest:{id:string;name:string;description:string};enabled:boolean;network_allowed:boolean;health:string;last_error?:string};
export type IndexedDocument = {source:string;title:string;chunk_count:number};
export type MemoryHit = {record:{id:number;text:string;metadata:unknown};distance:number};
export type ServiceResults = {
 status:{status:DaemonStatus}; memory:{status:MemoryStatus}; agents:{agents:Agent[]};
 runs:{runs:AgentRun[]}; connectors:{connectors:Connector[]}; documents:{documents:IndexedDocument[]};
};
export async function serviceData<K extends keyof ServiceResults>(section:K):Promise<ServiceResults[K]> {requireDesktop();return invoke('service_data',{section});}
export async function searchMemory(query:string) {requireDesktop();return invoke<{hits:MemoryHit[]}>('search_memory',{query});}
export type DocumentIngestResult = {source:string;chunks:number;unchanged?:boolean};
export type IndexSummary = {source:string;indexed:number;unchanged:number;skipped:number;failed:number;chunks:number;errors?:string[]};
export async function ingestDocument(path:string) {requireDesktop();return (await invoke<{result:DocumentIngestResult}>('ingest_document',{path})).result;}
export async function ingestDirectory(path:string,recursive:boolean) {requireDesktop();return (await invoke<{result:IndexSummary}>('ingest_directory',{path,recursive})).result;}
export async function ingestWebsite(url:string) {requireDesktop();return (await invoke<{result:IndexSummary}>('ingest_website',{url})).result;}
export async function startAgent(agentId:string,objective:string) {requireDesktop();return invoke<{run_id:string}>('start_agent',{agentId,objective});}
export async function cancelAgent(runId:string) {requireDesktop();return invoke('cancel_agent',{runId});}
export async function setConnector(connectorId:string,enabled:boolean,networkAllowed:boolean) {requireDesktop();return invoke('set_connector',{connectorId,enabled,networkAllowed});}
export type BrowserPage = {page_id:string;url:string;title:string;text:string;truncated:boolean;elements:{selector:string;tag:string;text:string;href?:string;type?:string}[]};
export type BrowserRequest = {action:'status'}|{action:'open';url:string}|{action:'navigate';page_id:string;url:string}|{action:'read'|'screenshot'|'close'|'back'|'forward'|'reload';page_id:string}|{action:'click';page_id:string;selector:string}|{action:'type';page_id:string;selector:string;text:string};
export async function browserRequest<T>(request:BrowserRequest) {requireDesktop();return invoke<T>('browser_request',{request});}
export async function confirmAgent(planId:string,approved:boolean) {requireDesktop();return invoke('confirm_agent',{planId,approved});}

export type ChatOptions={provider:string;model:string;useMemory:boolean;sources:string[]|null;settings:GenerationSettings};
export type StreamEvent={event:'started';request_id:string;provider:string;model?:string}|{event:'delta';request_id:string;content:string}|{event:'completed';request_id:string;response:ChatResponse}|{event:'failed';request_id:string;message:string}|{event:'cancelled';request_id:string};
export async function streamMessage(messages:Message[],options:ChatOptions,requestId:string,onEvent:(event:StreamEvent)=>void){
 requireDesktop();
 let terminalResolve:()=>void=()=>{};let failure='';
 const terminal=new Promise<void>(resolve=>{terminalResolve=resolve;});
 const channel=new Channel<StreamEvent>();
 channel.onmessage=event=>{onEvent(event);if(event.event==='failed')failure=event.message;if(['completed','failed','cancelled'].includes(event.event))terminalResolve();};
 const request={messages:messages.filter(m=>m.content.trim()||m.attachments?.some(a=>a.data_url)).map(({role,content,attachments})=>({role,content:content+(attachments||[]).filter(a=>!a.data_url).map(a=>`\n\n--- Attached document: ${a.name} ---\n${a.content}\n--- End document ---`).join(''),images:(attachments||[]).filter(a=>a.data_url).map(a=>a.data_url!)})),provider:options.provider||null,model:options.model||null,temperature:options.settings.temperature,max_tokens:options.settings.max_tokens,use_memory:options.useMemory,memory_sources:options.sources};
 await Promise.all([invoke<void>('stream_message',{request,requestId,onEvent:channel}),terminal]);
 if(failure)throw new Error(failure);
}
export async function stopMessage(requestId:string){return invoke<boolean>('stop_message',{requestId});}
export async function manage<T=Record<string,unknown>>(request:{type:string;[key:string]:unknown}){requireDesktop();return invoke<T>('manage_request',{request});}
export async function pickSource(kind:'document'|'folder',multiple=false){requireDesktop();return invoke<string[]>('pick_source',{kind,multiple});}
export async function readAttachment(path:string){requireDesktop();return invoke<Attachment>('read_attachment',{path});}
export async function loadConsoleState(){requireDesktop();return invoke<unknown>('load_console_state');}
export async function saveConsoleState(value:unknown){requireDesktop();return invoke<void>('save_console_state',{value});}
export async function legacyHistory(){requireDesktop();return invoke<unknown>('legacy_history');}
export type GpuStatus={available:boolean;message?:string;gpus:{name:string;utilization_percent:number|null;memory_used_mib:number|null;memory_total_mib:number|null}[]};
export async function gpuStatus(){requireDesktop();return invoke<GpuStatus>('gpu_status');}
export type VoiceResponse={status:string;message?:string;lease_id?:string;latest_sequence:number;events:{sequence:number;event:{type:string;value?:string|boolean}}[]};
export async function voiceStart(wakePhrase:string,blockedApplications:string[]){return invoke<VoiceResponse>('voice_start',{wakePhrase:wakePhrase||null,blockedApplications});}
export async function voicePoll(leaseId:string,afterSequence:number){return invoke<VoiceResponse>('voice_poll',{leaseId,afterSequence});}
export async function voiceStop(leaseId:string){return invoke<void>('voice_stop',{leaseId});}
export async function speak(text:string|null){return invoke('speak',{text});}
export type ProviderSettings={broker_available?:boolean;values:Record<string,string>;secrets:Record<string,string>};
export async function getProviderSettings(){return invoke<ProviderSettings>('provider_settings');}
export async function saveProviderSettings(settings:ProviderSettings){return invoke<void>('save_provider_settings',{settings});}
export async function restartBackend(){return invoke<void>('restart_ai_backend');}
export type IngestionSource={kind:'document';path:string}|{kind:'directory';path:string;recursive:boolean}|{kind:'website';url:string};
export type IngestionJob={id:string;created_at_unix_ms:number;source:IngestionSource;state:'queued'|'running'|'cancelling'|'cancelled'|'completed'|'failed';progress:IndexSummary;result?:IndexSummary;error?:string;cancel_requested:boolean};

export async function readJsonFile(path:string):Promise<Record<string,unknown>> {requireDesktop();return invoke('read_json_file',{path});}
export async function exportJsonFile(value:unknown,name:string):Promise<boolean> {requireDesktop();return invoke('export_json_file',{value,name});}
