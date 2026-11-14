const $ = id => document.getElementById(id);
let mode = 'create', format = 'zip', sources = [], busy = false;
const invoke = (command, args) => {
  if (!window.__TAURI__) return Promise.reject(new Error('Open the desktop app with cargo run to access local files.'));
  return window.__TAURI__.core.invoke(command, args);
};
function status(message = '', error = false) {
  $('status').textContent = message;
  $('status').className = error ? 'error' : 'success';
}
function render() {
  $('count').textContent = `${sources.length} ITEM${sources.length === 1 ? '' : 'S'}`;
  $('files').replaceChildren();
  for (const [index, path] of sources.entries()) {
    const li = document.createElement('li'), label = document.createElement('span'), remove = document.createElement('button');
    label.textContent = path; label.title = path; remove.textContent = '×'; remove.setAttribute('aria-label', `Remove ${path}`);
    remove.onclick = () => { sources.splice(index, 1); render(); };
    li.append(label, remove); $('files').append(li);
  }
}
function setMode(next) {
  if (busy) return;
  mode = next; sources = []; $('destination').value = ''; status(); render();
  const create = mode === 'create';
  for (const name of ['create', 'extract']) {
    $(`nav-${name}`).classList.toggle('active', mode === name);
    $(`tab-${name}`).classList.toggle('selected', mode === name);
  }
  $('title').textContent = create ? 'Good things, packed.' : 'Let it all unfold.';
  $('subtitle').textContent = create ? 'Gather your files. Make a little room.' : 'Open your archive. Get back to what’s inside.';
  $('source-title').innerHTML = `01 <span>${create ? 'Choose your files' : 'Choose your archive'}</span>`;
  $('drop-title').textContent = create ? 'A home for your files' : 'There’s more inside';
  $('drop-copy').textContent = create ? 'Click to select files, or drop them here' : 'Choose a ZIP, TAR, TAR.GZ, or GZ archive';
  $('format-field').hidden = !create; $('add-folder').hidden = !create;
  $('settings-title').textContent = create ? 'Make it yours' : 'Choose a destination';
  $('destination-label').textContent = create ? 'Save archive as' : 'Extract into folder';
  $('hint').textContent = create ? hints[format] : 'A new folder named after your archive will be created here.';
  $('run').textContent = create ? 'Create archive ↗' : 'Extract archive ↗';
}
const hints = {zip:'ZIP works almost everywhere. A good place to start.',tar:'Keep files together in an uncompressed archive.','tar.gz':'Bundle folders and files, then compress them with gzip.',gz:'Compress a single file. Choose TAR.GZ for folders or multiple files.'};
async function pick(kind) {
  try { return await invoke('choose', {kind}); } catch (e) { status(String(e), true); return []; }
}
async function add(kind) {
  const paths = await pick(kind);
  sources = mode === 'extract' ? paths.slice(0, 1) : [...new Set([...sources, ...paths])]; render();
}
for (const name of ['create', 'extract']) for (const prefix of ['nav', 'tab']) $(`${prefix}-${name}`).onclick = () => setMode(name);
$('dropzone').onclick = () => add(mode === 'create' ? 'files' : 'archive');
$('add-folder').onclick = () => add('folder');
$('clear').onclick = () => { sources = []; render(); status(); };
document.querySelectorAll('[data-format]').forEach(button => button.onclick = () => {
  format = button.dataset.format;
  document.querySelectorAll('[data-format]').forEach(b => b.classList.toggle('chosen', b === button));
  if ($('destination').value) $('destination').value = $('destination').value.replace(/\.(tar\.gz|zip|tar|gz)$/i, '') + '.' + format;
  $('hint').textContent = hints[format];
});
$('choose-destination').onclick = async () => {
  const paths = await pick(mode === 'create' ? 'save' : 'destination');
  if (paths[0]) $('destination').value = mode === 'create' ? paths[0].replace(/\.(tar\.gz|zip|tar|gz)$/i, '') + '.' + format : paths[0];
};
$('run').onclick = async () => {
  if (!sources.length || !$('destination').value) return status('Choose your source and destination first.', true);
  busy = true; document.body.classList.add('busy'); $('run').disabled = true;
  status(mode === 'create' ? 'Packing your files…' : 'Extracting your archive…');
  $('footer-note').textContent = 'Working locally…';
  try {
    const result = await invoke('process', {mode, sources, destination: $('destination').value, format});
    status(`${mode === 'create' ? 'Archive created' : 'Archive extracted'}: ${result}`);
    $('footer-note').textContent = 'All done. A little more organized.';
  } catch (e) { status(String(e), true); $('footer-note').textContent = 'Ready to try again.'; }
  finally { busy = false; document.body.classList.remove('busy'); $('run').disabled = false; }
};
if (window.__TAURI__) window.__TAURI__.event.listen('tauri://drag-drop', event => {
  if (busy) return;
  const paths = event.payload.paths || [];
  sources = mode === 'create' ? [...new Set([...sources, ...paths])] : paths.slice(0, 1);
  render();
}).catch(e => status(String(e), true));
