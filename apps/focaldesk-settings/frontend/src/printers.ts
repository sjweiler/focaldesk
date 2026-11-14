export interface Printer {name:string;enabled:boolean;accepting_jobs:boolean|null;is_default:boolean;state:string;device_uri:string|null}
export interface DiscoveredPrinter {id:string;kind:string;uri:string;suggested_name:string}
export interface PrinterSnapshot {scheduler_running:boolean;printers:Printer[];discovered:DiscoveredPrinter[];drivers:{id:string;name:string}[];warnings:string[]}
export interface PrintJob {id:string;printer:string;owner:string;bytes:number;submitted:string}
export type PrinterAction={type:'discover'|'load_drivers'}|{type:'install';name:string;uri:string;model:string}|{type:'install_discovered';id:string;name:string;model:string}|{type:'set_default';name:string}|{type:'set_enabled';name:string;enabled:boolean}|{type:'set_accepting';name:string;accepting:boolean}|{type:'cancel_job';printer:string;id:string};
