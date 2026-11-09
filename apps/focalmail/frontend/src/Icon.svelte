<script lang="ts">
 import {paths, type IconName} from './icons';
 let {name}: {name:IconName}=$props();
</script>
<svg class="icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{#each paths[name] as d}<path {d}/>{/each}</svg>
