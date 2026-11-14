export interface CalendarAccount {account_id:string;provider:string;connected:boolean;enabled:boolean;cached_events:number;sync_state:string;calendar_ids:string[];reminder_minutes:number;last_sync_at_unix?:number|null;last_error?:string|null}
export interface CalendarEvent {account_id:string;provider:string;calendar_id:string;event_id:string;title:string;start_time:string;end_time:string;organizer:string;all_day:boolean}
export interface CalendarFlow {flow_id:string;account_id:string;provider:string;state:string;user_code:string|null;browser_available:boolean;message:string}
export interface CalendarSnapshot {accounts:CalendarAccount[];events:CalendarEvent[];flow:CalendarFlow|null;paused:boolean}
export interface CalendarPreferences {account:CalendarAccount;calendars:{id:string;name:string;primary:boolean;selected:boolean}[]}
export type CalendarAction={type:'sync';account_id:string}|{type:'disconnect';account_id:string;delete_local_data:boolean}|{type:'preferences';account_id:string;expected_calendar_ids:string[];expected_reminder_minutes:number;calendar_ids:string[];reminder_minutes:number};
export function eventTime(event:CalendarEvent,locale?:string):string {
  if(event.all_day){const date=new Date(event.start_time.slice(0,10)+'T12:00:00Z');return Number.isNaN(date.getTime())?event.start_time:date.toLocaleDateString(locale,{timeZone:'UTC',weekday:'short',month:'short',day:'numeric'})+' · All day';}
  const start=new Date(event.start_time),end=new Date(event.end_time);
  if(Number.isNaN(start.getTime()))return event.start_time;
  return start.toLocaleString(locale,{weekday:'short',month:'short',day:'numeric',hour:'numeric',minute:'2-digit'})+(Number.isNaN(end.getTime())?'':' – '+end.toLocaleTimeString(locale,{hour:'numeric',minute:'2-digit'}));
}
export function sameSelection(a:string[],b:string[]):boolean{const sorted=[...b].sort();return a.length===b.length&&[...a].sort().every((id,index)=>id===sorted[index]);}
