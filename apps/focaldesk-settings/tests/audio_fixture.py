"""Private command fixtures: exercise real Rust pactl/parec processes, no hardware."""
import json
from pathlib import Path

PACTL = r'''#!/usr/bin/env python3
import json,os,sys,time
from pathlib import Path
root=Path(os.environ['FOCALDESK_AUDIO_FIXTURE'])
args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as log: log.write(json.dumps(args)+'\n')
if (root/'hang').exists(): time.sleep(5)
if (root/'offline').exists(): print('Fixture audio server unavailable',file=sys.stderr);sys.exit(1)
s=json.loads((root/'audio.json').read_text())
kind=None
if args==['--format=json','info']:
 print(json.dumps({'server_name':'PulseAudio (on PipeWire fixture)','default_sink_name':s['default_output'],'default_source_name':s['default_input']}));sys.exit()
if len(args)==3 and args[:2]==['--format=json','list']:
 print(json.dumps(s['outputs' if args[2]=='sinks' else 'inputs']));sys.exit()
if (root/'reject').exists(): print('Fixture rejected audio change',file=sys.stderr);sys.exit(1)
command=args[0]
kind='outputs' if 'sink' in command else 'inputs'
device=next((x for x in s[kind] if str(x['index'])==args[1] or x['name']==args[1]),None)
if device is None: print('Device disappeared',file=sys.stderr);sys.exit(1)
if command.startswith('set-default-'): s['default_output' if kind=='outputs' else 'default_input']=device['name']
elif command.endswith('-volume'):
 value=round(float(args[2].rstrip('%'))/100*65536)
 for channel in device['volume'].values(): channel['value']=value
elif command.endswith('-mute'): device['mute']=args[2]=='1'
else: print('Unsupported fixture operation',file=sys.stderr);sys.exit(1)
temporary=root/f'audio-{os.getpid()}.tmp'
temporary.write_text(json.dumps(s))
temporary.replace(root/'audio.json')
'''
PAREC = r'''#!/usr/bin/env python3
import json,os,signal,struct,sys,time
from pathlib import Path
root=Path(os.environ['FOCALDESK_AUDIO_FIXTURE'])
with (root/'capture.jsonl').open('a') as log: log.write(json.dumps({'pid':os.getpid(),'args':sys.argv[1:]})+'\n')
def stopped(signum,frame):
 with (root/'capture-stopped.jsonl').open('a') as log: log.write(json.dumps({'pid':os.getpid(),'signal':signum})+'\n')
 sys.exit(0)
signal.signal(signal.SIGTERM,stopped)
if (root/'capture-fail').exists(): sys.exit(1)
try:
 while True:
  if not (root/'capture-stall').exists(): os.write(1,struct.pack('<f',0.25)*400)
  time.sleep(.025)
except (BrokenPipeError,OSError): pass
'''

PAPLAY = r'''#!/usr/bin/env python3
import json,os,sys,time,wave
from pathlib import Path
root=Path(os.environ['FOCALDESK_AUDIO_FIXTURE'])
with wave.open(sys.argv[-1]) as audio:
 assert audio.getnchannels()==2 and audio.getframerate()==44100
 assert audio.getnframes()==44100
with (root/'speaker.jsonl').open('a') as log: log.write(json.dumps({'pid':os.getpid(),'args':sys.argv[1:]})+'\n')
if (root/'speaker-fail').exists(): sys.exit(1)
time.sleep(30 if (root/'speaker-hang').exists() else 1)
'''


def install(root: Path):
    root.mkdir(parents=True, exist_ok=True)
    binary = root / 'bin'
    binary.mkdir()
    for name, source in [('pactl', PACTL), ('parec', PAREC), ('paplay', PAPLAY)]:
        path = binary / name
        path.write_text(source)
        path.chmod(0o755)
    def device(index, name, label, value, muted=False):
        return {'index': index, 'name': name, 'description': label, 'mute': muted,
                'volume': {'mono': {'value': value}}, 'active_port': None}
    state = {'default_output': 'studio.output', 'default_input': 'usb.mic',
             'outputs': [device(7, 'studio.output', 'Studio Speakers', 32768),
                         device(8, 'headphones.output', 'USB Headphones', 16384)],
             'inputs': [device(3, 'usb.mic', 'USB Microphone', 49152),
                        device(4, 'webcam.mic', 'Webcam Microphone', 32768),
                        device(5, 'studio.output.monitor', 'Monitor of Studio Speakers', 65536)]}
    (root / 'audio.json').write_text(json.dumps(state))
    return binary
