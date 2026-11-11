export interface Attachment {name:string;size:number;id:string;data:string}
export interface Span {text:string;bold?:boolean;italic?:boolean;code?:boolean;heading?:number;link?:string|null}
export interface Message {
 id:string;account:string;sender:string;email:string;subject:string;body:string;date:string;folder:string;labels:string[];
 pending_move?:boolean;timestamp:number;unread:boolean;flagged:boolean;html?:{inline_attempted?:boolean;layout?:string|null;images?:{index:number;alt:string;source:string|null;inline_data:string|null}[];blocks:{spans:Span[]}[];blocked_images:number;limited:boolean}|null;
 html_loaded?:boolean;attachments_loaded?:boolean;attachments:Attachment[];to_header?:string;cc_header?:string;
}
export interface Signature {html:string;new_messages:boolean;replies_forwards:boolean}
export interface Draft {html_body?:string|null;signature_initialized?:boolean;id:string;from:string;to:string;cc:string;subject:string;body:string;forward?:{attachments:Attachment[]}|null;[key:string]:unknown}
export interface DraftFile {name:string;size:number}
export interface DraftRecord {draft:Draft;files:DraftFile[]}
export type SyncHistory = 'one_month'|'three_months'|'six_months'|'one_year'|'all';
export interface AccountStatus {sync_history?:SyncHistory;email:string;provider:string;state:string;detail:string;cached_count?:number;sync_paused?:boolean;service_available?:boolean;search_pending?:number;progress?:{phase:string;completed:number;total:number|null;updated:number}|null}
export interface Snapshot {folders?:Record<string,string[]>;messages:Message[];accounts:string[];drafts:{id:string;subject:string;to:string;from:string}[];active?:string|null;notifications?:boolean;status?:AccountStatus[]}
export type Action='Archive'|'Trash'|{Read:boolean}|{Star:boolean};
declare global {
 interface Window {__TAURI__?:{core:{invoke:<T=unknown>(command:string,args?:Record<string,unknown>)=>Promise<T>};event:{listen:<T=string>(event:string,callback:(event:{payload:T})=>void)=>Promise<()=>void>}}}
}

export interface DeviceAuthorization {user_code:string;expires_in:number;verification_uri?:string|null}

export interface MailTarget {account:string;id:string}
export interface BulkOutcome {updated:MailTarget[];failed:{target:MailTarget;detail:string}[];not_attempted:MailTarget[];warnings:string[];cancelled:boolean}
