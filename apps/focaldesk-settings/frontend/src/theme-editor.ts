export type ColorSpace='srgb'|'display_p3'|'rec2020';
export interface ThemeColor {space:ColorSpace;r:number;g:number;b:number;a:number}
export interface GradientStop {position:number;color:ThemeColor}
export type Paint={mode:'solid';color:ThemeColor}|{mode:'linear_gradient';angle:number;interpolation:{space:ColorSpace;premultiplied_alpha:boolean};stops:GradientStop[]}|{mode:'radial_gradient';center:[number,number];radius:number;interpolation:{space:ColorSpace;premultiplied_alpha:boolean};stops:GradientStop[]};
export type SurfaceName='bar'|'dock'|'button'|'active_button'|'popup'|'window_frame';
export type Interaction='normal'|'hover'|'pressed'|'selected'|'focused'|'urgent'|'disabled';
export type Surface={normal:ThemeColor}&Partial<Record<Exclude<Interaction,'normal'>,ThemeColor|null>>;
export interface ThemeDocument {
  format_version:number;name:string;paint:Paint;dynamic_range:'sdr'|'hdr';hdr_luminance_nits:number;
  wallpaper:{path:string|null;fit:'fill'|'fit'|'stretch'|'center'|'tile';tint:ThemeColor|null;dim:number};
  backing:{rail_opacity:number;dock_opacity:number};
  semantic:{surfaces:Record<SurfaceName,Surface>;edges:{border_color:ThemeColor;inner_highlight:ThemeColor;border_width:number;shadow:number;glow:number;radius:number};typography:{primary:ThemeColor;secondary:ThemeColor;font_weight:number;size:number;letter_spacing:number};layout:{bar_height:number;dock_width:number;padding:number;gap:number;icon_size:number};color_behavior:{gamut:ColorSpace;dynamic_range:'sdr'|'hdr';sdr_white_nits:number;luminance_cap_nits:number;gamut_mapping:'clip'|'perceptual'|'preserve_hue'};wallpaper:{blur:number;saturation:number;automatic_accent:boolean}};
}
export interface Projection {wallpaper:string|null;wallpaper_tint:string|null;paint:string;surface:string;text:string;accent:string;contrast:{surface:string;state:string;ratio:number;required:number}[]}
export interface EditorDocument {document:ThemeDocument;source:{path:string;hash:string}|null;toml:string;preview:Projection}
export interface RuntimeStatus {preview_active:boolean;applied_revision:number;gradient_rendering:boolean;semantic_rendering:boolean;contrast_issue_count:number}
export function switchPaint(paint:Paint,mode:Paint['mode']):Paint {
  const copy=<T>(value:T):T=>JSON.parse(JSON.stringify(value));
  const color=copy(paint.mode==='solid'?paint.color:paint.stops[0].color);
  if(mode==='solid')return {mode,color};
  const stops=paint.mode==='solid'?[{position:0,color},{position:1,color:copy(color)}]:copy(paint.stops);
  const interpolation=paint.mode==='solid'?{space:'srgb' as const,premultiplied_alpha:true}:copy(paint.interpolation);
  return mode==='linear_gradient'?{mode,angle:135,interpolation,stops}:{mode,center:[.5,.5],radius:.75,interpolation,stops};
}
export function colorHex(color:ThemeColor):string {
  const encode=(v:number)=>{v=Math.max(0,Math.min(1,v));return Math.round((v<=.0031308?12.92*v:1.055*Math.pow(v,1/2.4)-.055)*255).toString(16).padStart(2,'0');};
  return '#'+encode(color.r)+encode(color.g)+encode(color.b);
}
export function hexColor(hex:string,alpha=1):ThemeColor {
  const decode=(s:string)=>{const v=parseInt(s,16)/255;return v<=.04045?v/12.92:Math.pow((v+.055)/1.055,2.4);};
  return {space:'srgb',r:decode(hex.slice(1,3)),g:decode(hex.slice(3,5)),b:decode(hex.slice(5,7)),a:alpha};
}
