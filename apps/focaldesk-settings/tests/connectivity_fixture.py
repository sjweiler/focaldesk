"""Private nmcli/BlueZ fixtures; production Rust still executes normal tools."""
import json
from pathlib import Path

PROGRAM = r'''#!/usr/bin/env python3
import fcntl,json,os,sys,time
from pathlib import Path
root=Path(os.environ['FOCALDESK_CONNECTIVITY_FIXTURE'])
tool=Path(sys.argv[0]).name
args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as log: log.write(json.dumps({'tool':tool,'args':args,'pid':os.getpid()})+'\n')
def load(): return json.loads((root/'state.json').read_text())
def save(s):
 p=root/f'state-{os.getpid()}.tmp';p.write_text(json.dumps(s));p.replace(root/'state.json')
def update(change):
 with (root/'lock').open('w') as lock:
  fcntl.flock(lock,fcntl.LOCK_EX);s=load();change(s);save(s)
if (root/f'{tool}-offline').exists(): print('Fixture service unavailable',file=sys.stderr);sys.exit(1)
if (root/f'{tool}-hang').exists(): time.sleep(30)
if tool=='nmcli':
 s=load()
 if args==['-t','-f','WIFI','radio']: print('enabled' if s['wifi'] else 'disabled');sys.exit()
 if args==['-t','-f','DEVICE,TYPE,STATE,CONNECTION','device','status']:
  print('eth0:ethernet:'+('connected:Wired' if s['wired'] else 'disconnected:--'))
  print('wlan0:wifi:'+('connected:Office' if s['wifi_connected'] else 'disconnected:--'));sys.exit()
 if '--rescan' in args:
  def escape(value): return value.replace('\\','\\\\').replace(':','\\:')
  for index,(ssid,bssid,security,signal) in enumerate([('Office: West','11:22:33:44:55:66','WPA2',85),('Guest','11:22:33:44:55:77','--',60)]):
   print(':'.join([('*' if s['wifi_connected']==bssid else ''),escape(ssid),escape(bssid),security,str(signal),'wlan0']))
  sys.exit()
 if (root/'nmcli-reject').exists(): print('Fixture denied network change',file=sys.stderr);sys.exit(1)
 if args[:2]==['radio','wifi']: update(lambda s:s.update(wifi=args[2]=='on'));sys.exit()
 if args[-3:]==['device','wifi','rescan']: update(lambda s:s.update(scans=s['scans']+1));sys.exit()
 if 'connect' in args and 'wifi' in args:
  secret=sys.stdin.readline().rstrip('\n')
  if secret not in ['', 'fixture-password']: print('Rejected secret '+secret);sys.exit(1)
  bssid=args[args.index('connect')+1]
  update(lambda s:s.update(wifi_connected=bssid,secret_received=secret=='fixture-password'));sys.exit()
 if 'device' in args:
  target=args[-1];connected='connect' in args
  update(lambda s:s.update(**({'wired':connected} if target=='eth0' else {'wifi_connected':None if not connected else '11:22:33:44:55:66'})));sys.exit()
 print('Unsupported nmcli fixture request',file=sys.stderr);sys.exit(1)
if args:
 s=load()
 if args==['show']: print('Controller AA:BB:CC:DD:EE:00\n\tPowered: '+('yes' if s['bluetooth'] else 'no'));sys.exit()
 if args[0]=='devices':
  for d in s['devices']:
   if len(args)==1 or d.get(args[1].lower(),False): print('Device '+d['address']+' '+d['name'])
  sys.exit()
 if args[0]=='info':
  d=next(d for d in s['devices'] if d['address']==args[1]);print('Paired: '+('yes' if d['paired'] else 'no')+'\nBonded: '+('yes' if d['paired'] else 'no'));sys.exit()
 if (root/'bluetoothctl-reject').exists(): print('Failed to change device: fixture rejection');sys.exit()
 if args[0]=='power': update(lambda s:s.update(bluetooth=args[1]=='on'));sys.exit()
 if args[0] in ['connect','disconnect','remove']:
  def change(s):
   d=next(d for d in s['devices'] if d['address']==args[1])
   if args[0]=='remove': d.update(paired=False,connected=False)
   else: d['connected']=args[0]=='connect'
  update(change);print('Successful');sys.exit()
 print('Unsupported Bluetooth fixture request',file=sys.stderr);sys.exit(1)
address=None
for line in sys.stdin:
 line=line.strip()
 with (root/'agent.jsonl').open('a') as log: log.write(json.dumps({'pid':os.getpid(),'command':line})+'\n')
 if line.startswith('agent '): print('Agent registered',flush=True)
 elif line=='default-agent': print('Default agent request successful',flush=True)
 elif line.startswith('pair '):
  address=line.split(' ',1)[1]
  print('[agent] Enter PIN code:' if (root/'pin').exists() else '[agent] Confirm passkey 123456 (yes/no):',flush=True)
 elif line=='scan on': print('Discovery started',flush=True)
 elif line=='quit': break
 elif line in ['yes','1234'] and address:
  def paired(s):
   next(d for d in s['devices'] if d['address']==address)['paired']=True
  update(paired);print('Pairing successful',flush=True)
 elif line=='no': print('Failed to pair: AuthenticationRejected',flush=True)
'''

def install(root: Path):
    root.mkdir(parents=True)
    binary=root/'bin'
    binary.mkdir()
    for name in ['nmcli','bluetoothctl']:
        path=binary/name
        path.write_text(PROGRAM)
        path.chmod(0o755)
    (root/'state.json').write_text(json.dumps({'wifi':True,'wired':True,'wifi_connected':None,'scans':0,'secret_received':False,'bluetooth':True,'devices':[
        {'address':'AA:BB:CC:DD:EE:01','name':'Studio headphones','paired':True,'connected':True},
        {'address':'AA:BB:CC:DD:EE:02','name':'Desk keyboard','paired':False,'connected':False},
    ]}))
    return binary
