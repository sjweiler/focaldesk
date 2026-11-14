export interface NetworkDevice { name:string; kind:string; state:string; connection:string }
export interface WifiNetwork { id:string; device:string; bssid:string; ssid:string; security:string; signal:number; active:boolean }
export interface NetworkSnapshot { wifi_enabled:boolean; devices:NetworkDevice[]; networks:WifiNetwork[] }
export type NetworkAction = {type:'set_wifi';enabled:boolean}|{type:'rescan'}|{type:'set_device';device:string;connected:boolean}|{type:'connect_wifi';id:string;password:string};
export interface BluetoothDevice {address:string;name:string;paired:boolean;connected:boolean}
export interface BluetoothView {lease:number;snapshot:{powered:boolean;scanning:boolean;devices:BluetoothDevice[];error:string|null};pairing:{address:string;phase:string;prompt:string|null;kind:string|null;message:string}|null}
export type BluetoothAction = {type:'set_power';enabled:boolean}|{type:'scan'|'stop_scan'|'cancel_pairing'}|{type:'pair'|'connect'|'disconnect'|'forget';address:string}|{type:'reply';value:string};
