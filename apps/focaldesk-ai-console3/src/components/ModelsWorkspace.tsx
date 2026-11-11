import {useState} from 'react';
import {listModels,type Provider} from '../services';
import {ServiceWorkspace} from './ServiceWorkspace';
export function ModelsWorkspace({providers,onRefresh,onSelect}:{providers:Provider[];onRefresh:()=>void;onSelect:(provider:string,model:string)=>void}){
 const [models,setModels]=useState<Record<string,{id:string}[]>>({}),[error,setError]=useState(''),[loading,setLoading]=useState('');
 async function load(provider:string){setLoading(provider);setError('');try{const result=await listModels(provider);setModels(current=>({...current,[provider]:result}));}catch(e){setError(String(e));}finally{setLoading('');}}
 return <div><div className="service-toolbar"><h3>Providers and models</h3><button className="page-action" onClick={onRefresh}>Refresh providers</button></div>{error&&<p className="service-error">{error}</p>}{providers.map(p=><section className="panel" key={p.id}><h3>{p.id} · {p.kind}</h3><p>Default: {p.default_model||'Provider-selected model'}</p><button className="page-action" disabled={!!loading} onClick={()=>void load(p.id)}>{loading===p.id?'Loading…':'List models'}</button><button className="page-action" onClick={()=>onSelect(p.id,'')}>Use default</button>{models[p.id]?.map(m=><div className="history-row" key={m.id}><span>{m.id}</span><button className="page-action" onClick={()=>onSelect(p.id,m.id)}>Use model</button></div>)}</section>)}<ServiceWorkspace section="Models"/><p>Provider endpoints, default models and credentials are configured in Settings.</p></div>;
}
