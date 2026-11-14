export interface WindowRule {app_id:string;title_contains:string;workspace:number|null;output:string|null;floating:boolean|null;maximize:boolean|null}
export interface StartupApp {name:string;command:string;arguments:string[];enabled:boolean}
export interface AppSettings {terminal:string;browser:string;browser_launch_backend:'auto'|'wayland'|'xwayland';file_manager:string;email:string;pin_email_to_shelf:boolean;window_rules:WindowRule[];startup_apps:StartupApp[]}
export interface ApplicationsSnapshot {apps:AppSettings;defaults:AppSettings}
export interface AssociationSnapshot {mime_type:string;description:string;current:string|null;version:string;applications:{id:string;name:string;description:string;executable:string;recommended:boolean}[]}
export function moveEntry<T>(entries:T[],origins:(number|null)[],index:number,direction:number):void {const to=index+direction;if(to<0||to>=entries.length)return;[entries[index],entries[to]]=[entries[to],entries[index]];[origins[index],origins[to]]=[origins[to],origins[index]];}
