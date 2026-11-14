import { mount } from 'svelte';
import App from './App.svelte';
import '@focaldesk/theme/tokens.css';
import './style.css';
mount(App, {target: document.getElementById('app')!});
