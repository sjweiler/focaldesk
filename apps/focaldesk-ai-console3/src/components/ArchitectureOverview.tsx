import {Monitor, Folder, Settings, Sparkles, Layers, Cpu, Box} from 'lucide-react';
export function ArchitectureOverview() {
  return <figure className="architecture" aria-label="FocalDesk architecture example">
    <figcaption><Layers size={20}/> Architecture <small>Illustrative overview</small></figcaption>
    <div className="architecture-node session"><Monitor size={20}/><span>FocalDesk Desktop (Session)</span></div>
    <div className="architecture-apps">
      <div className="architecture-node"><Folder size={22}/><span>Files<small>GTK4</small></span></div>
      <div className="architecture-node"><Settings size={22}/><span>Settings<small>GTK4</small></span></div>
      <div className="architecture-node console"><Sparkles size={22}/><span>AI Console<small>Tauri 2</small></span></div>
    </div>
    <div className="architecture-node services"><Box size={25}/><span>FocalDesk Services (Rust)<small>Audio · Bluetooth · Voice · Portal · IPC · Memory · RAG</small></span></div>
    <div className="architecture-apps foundation">
      <div className="architecture-node"><Layers size={24}/><span>Compositor / Engine<small>Smithay · Wayland · Vulkan</small></span></div>
      <div className="architecture-node"><Cpu size={24}/><span>System Integration<small>PipeWire · KMS / DRM</small></span></div>
    </div>
  </figure>;
}
