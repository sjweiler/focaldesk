export type GenerationSettings={temperature:number;max_tokens:number;read_aloud:boolean;theme:string;wake_phrase:string;blocked_applications:string[]};
export const defaultSettings:GenerationSettings={temperature:0.3,max_tokens:2048,read_aloud:false,theme:'midnight',wake_phrase:'',blocked_applications:[]};
export function normalizeSettings(value:unknown):GenerationSettings{
 const v=(value&&typeof value==='object'?value:{}) as Partial<GenerationSettings>;
 return {temperature:typeof v.temperature==='number'&&Number.isFinite(v.temperature)?Math.min(2,Math.max(0,v.temperature)):defaultSettings.temperature,max_tokens:typeof v.max_tokens==='number'&&Number.isFinite(v.max_tokens)?Math.min(32768,Math.max(64,Math.round(v.max_tokens))):defaultSettings.max_tokens,read_aloud:v.read_aloud===true,theme:['midnight','slate','contrast'].includes(v.theme||'')?v.theme!:'midnight',wake_phrase:typeof v.wake_phrase==='string'?v.wake_phrase.slice(0,100):'',blocked_applications:Array.isArray(v.blocked_applications)?v.blocked_applications.filter((s):s is string=>typeof s==='string').slice(0,64):[]};
}
