import type {Message} from './types';
export function mailDate(m:Pick<Message,'timestamp'|'date'>):Date|null {
 // The mailbox index sorts this authoritative instant. Guessing from a raw
 // Date header here would display a different date than the one used to sort.
 if(!Number.isFinite(m.timestamp)||m.timestamp<=0)return null;
 const date=new Date(m.timestamp);
 return Number.isFinite(date.getTime())?date:null;
}
export function dateLabels(m:Pick<Message,'timestamp'|'date'>,now=new Date()){
 const date=mailDate(m);
 if(!date)return {key:'unknown',group:'No date',short:'No date',full:'Date unavailable',iso:''};
 const day=new Date(date.getFullYear(),date.getMonth(),date.getDate());
 const today=new Date(now.getFullYear(),now.getMonth(),now.getDate());
 const yesterday=new Date(now.getFullYear(),now.getMonth(),now.getDate()-1);
 const group=day.getTime()===today.getTime()?'Today':day.getTime()===yesterday.getTime()?'Yesterday':new Intl.DateTimeFormat(undefined,{weekday:'short',month:'short',day:'numeric',...(date.getFullYear()!==now.getFullYear()?{year:'numeric' as const}:{})}).format(date);
 return {key:`${date.getFullYear()}-${date.getMonth()}-${date.getDate()}`,group,
  short:new Intl.DateTimeFormat(undefined,{hour:'numeric',minute:'2-digit'}).format(date),
  full:new Intl.DateTimeFormat(undefined,{year:'numeric',month:'short',day:'numeric',hour:'numeric',minute:'2-digit',timeZoneName:'short'}).format(date),iso:date.toISOString()};
}
