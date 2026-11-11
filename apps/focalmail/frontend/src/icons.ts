// Local SVG primitives. Sender content never enters this icon renderer.
export const paths = {
 mail: ['M3 5h18v14H3z','m3 6 9 7 9-7'],
 inbox: ['M4 4h16l2 11v5H2v-5z','M2 15h6l2 3h4l2-3h6'],
 send: ['m22 2-7 20-4-9-9-4Z','M22 2 11 13'],
 edit: ['m16 3 5 5-12 12-6 1 1-6Z','m13 6 5 5'],
 file: ['M14 2H5v20h14V7z','M14 2v6h5','M8 13h8M8 17h5'],
 trash: ['M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7M14 10v7'],
 shield: ['M12 2 3 6v6c0 5 9 10 9 10s9-5 9-10V6z','m8 12 3 3 5-6'],
 archive: ['M3 3h18v5H3zM5 8v13h14V8M9 12h6'],
 star: ['m12 2 3.1 6.3 6.9 1-5 4.9 1.2 6.8-6.2-3.2-6.2 3.2L7 14.2l-5-4.9 6.9-1Z'],
 unread: ['M3 5h18v14H3z','m3 6 9 7 9-7','M18 2h4v4'],
 reply: ['m9 5-7 7 7 7','M2 12h12a7 7 0 0 1 7 7'],
 replyAll: ['m7 5-6 7 6 7','m13 5-6 7 6 7','M7 12h8a7 7 0 0 1 7 7'],
 forward: ['m15 5 7 7-7 7','M22 12H10a7 7 0 0 0-7 7'],
 search: ['M20 20l-5-5','M17 10a7 7 0 1 1-14 0 7 7 0 0 1 14 0'],
 refresh: ['M21 4v6h-6M3 20v-6h6','M4 9a8 8 0 0 1 13-5l4 6M3 14l4 6a8 8 0 0 0 13-5'],
 settings: ['M9 3h6l1 4 4 1 2 5-3 3 1 4-5 2-3-3-4 1-3-5 2-3-1-4 4-1Z','M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0'],
 plus: ['M12 5v14M5 12h14'],
 chevron: ['m9 5 7 7-7 7'],
 close: ['m6 6 12 12M6 18 18 6'],
 lock: ['M5 10h14v11H5z','M8 10V6a4 4 0 0 1 8 0v4'],
 paperclip: ['m8 12 7-7a4 4 0 0 1 6 6L10 22a6 6 0 0 1-8-8L13 3','m6 15 9-9a1 1 0 0 1 3 3l-9 9'],
} as const;
export type IconName = keyof typeof paths;
export function icon(name: IconName) {
 const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');
 for(const [key,value] of Object.entries({viewBox:'0 0 24 24',fill:'none',stroke:'currentColor','stroke-width':'1.6','stroke-linecap':'round','stroke-linejoin':'round','aria-hidden':'true',class:'icon'}))svg.setAttribute(key,value);
 for(const d of paths[name]){const p=document.createElementNS(svg.namespaceURI,'path');p.setAttribute('d',d);svg.append(p);}
 return svg;
}
