<script lang="ts">
  import { Handle, Position, type NodeProps } from '@xyflow/svelte';
  import { catalog, subtitle, type FlowNode } from './model';
  let { data, selected }: NodeProps<FlowNode> = $props();
  let meta = $derived(catalog.find(c=>c.kind === data.kind)!);
</script>
<div class:chosen={selected} class="flow-block {meta.color}" class:decision={data.kind==='condition'}>
  {#if data.kind!=='schedule' && data.kind!=='webhook'}<Handle type="target" position={Position.Top}/>{/if}
  <span class="block-icon {meta.color}">{meta.icon}</span>
  <div class="block-copy"><strong>{meta.label}</strong><small>{subtitle(data)}</small></div>
  <span class="block-edit">↗</span>
  {#if data.kind==='condition'}
    <Handle type="source" id="yes" position={Position.Bottom} style="left:28%"/>
    <Handle type="source" id="no" position={Position.Bottom} style="left:72%"/>
    <span class="branch yes">Yes</span><span class="branch no">No</span>
  {:else}<Handle type="source" position={Position.Bottom}/>{/if}
</div>
