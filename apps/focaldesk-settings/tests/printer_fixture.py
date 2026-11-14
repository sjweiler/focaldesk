"""Private CUPS executables. No real printers, print jobs, or server are used."""
import json
from pathlib import Path

PROGRAM=r'''#!/usr/bin/env python3
import fcntl,json,os,sys,time
from pathlib import Path
root=Path(os.environ['FOCALDESK_PRINTER_FIXTURE'])
tool=Path(sys.argv[0]).name
args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as log: log.write(json.dumps({'tool':tool,'args':args})+'\n')
if (root/(tool+'-hang')).exists(): time.sleep(30)
if (root/'offline').exists(): print('Fixture CUPS service unavailable',file=sys.stderr);sys.exit(1)
def load():return json.loads((root/'state.json').read_text())
def save(s):
 path=root/f'state-{os.getpid()}.tmp';path.write_text(json.dumps(s));path.replace(root/'state.json')
def mutate(change):
 with (root/'lock').open('w') as lock:
  fcntl.flock(lock,fcntl.LOCK_EX);s=load();change(s);save(s)
s=load()
if tool=='lpstat':
 if args==['-r']: print('scheduler is running' if s['running'] else 'scheduler is not running');sys.exit()
 if args==['-d']:
  if not s['default']: print('lpstat: No system default destination.',file=sys.stderr);sys.exit(1)
  print('system default destination: '+s['default']);sys.exit()
 if args in [['-p'],['-a'],['-v']]:
  if not s['printers']: print('lpstat: No destinations added.',file=sys.stderr);sys.exit(1)
  if args==['-a'] and (root/'accepting-error').exists(): print('Fixture acceptance state unavailable',file=sys.stderr);sys.exit(1)
  for printer in s['printers']:
   name=printer['name']
   if args==['-p']: print('printer '+name+(' is idle. enabled since Thu Nov 12 2026' if printer['enabled'] else ' disabled since Thu Nov 12 2026 - paused'))
   elif args==['-a']: print(name+(' accepting requests since Thu Nov 12 2026' if printer['accepting'] else ' not accepting requests since Thu Nov 12 2026'))
   else: print('device for '+name+': '+printer['uri'])
  sys.exit()
 if args[:3]==['-W','not-completed','-o']:
  if (root/'vanish-job').exists():
   (root/'vanish-job').unlink();mutate(lambda s:s.update(jobs=[]));sys.exit()
  for job in s['jobs']:
   if job['printer']==args[3]: print(job['id']+' fixture-user 4096 Thu Nov 12 2026 09:00:00')
  sys.exit()
elif tool=='lpinfo':
 if args==['-v']:
  print('network ipp\nnetwork ipps\ndirect vendorprint\nnetwork '+s['discovered_uri']);sys.exit()
 if args==['-m']: print('drv:///fixture.drv/model.ppd Fixture Legacy Driver');sys.exit()
else:
 if (root/'reject').exists(): print('Fixture CUPS authorization denied',file=sys.stderr);sys.exit(1)
 if tool=='lpadmin' and args[:1]==['-d']:
  mutate(lambda s:s.update(default=args[1]));sys.exit()
 if tool=='lpadmin' and args[:1]==['-p']:
  name=args[1];uri=args[args.index('-v')+1];model=args[args.index('-m')+1]
  def add(s):s['printers'].append({'name':name,'uri':uri,'model':model,'enabled':True,'accepting':True})
  mutate(add);sys.exit()
 if tool in ['cupsenable','cupsdisable','cupsaccept','cupsreject']:
  def update(s):
   printer=next(printer for printer in s['printers'] if printer['name']==args[0])
   printer['enabled' if tool in ['cupsenable','cupsdisable'] else 'accepting']=tool in ['cupsenable','cupsaccept']
  mutate(update);sys.exit()
 if tool=='cancel':
  def cancel(s):s['jobs']=[job for job in s['jobs'] if job['id']!=args[0]];s['cancelled'].append(args[0])
  mutate(cancel);sys.exit()
print('Unsupported fixture CUPS request',file=sys.stderr);sys.exit(1)
'''

def install(root:Path):
    root.mkdir(parents=True)
    binary=root/'bin'
    binary.mkdir()
    for name in ['lpstat','lpinfo','lpadmin','cupsenable','cupsdisable','cupsaccept','cupsreject','cancel']:
        path=binary/name
        path.write_text(PROGRAM)
        path.chmod(0o755)
    (root/'state.json').write_text(json.dumps({'running':True,'default':'Office-Printer','discovered_uri':'ipp://new-printer.local/ipp/print','printers':[
        {'name':'Office-Printer','uri':'ipp://user:fixture-uri-secret@office.local/ipp/print?password=hidden','enabled':True,'accepting':True},
        {'name':'Photo_Printer','uri':'usb://Fixture/Photo?serial=123','enabled':False,'accepting':False},
    ],'jobs':[{'id':'Office-Printer-42','printer':'Office-Printer'},{'id':'Office-Printer-43','printer':'Office-Printer'}],'cancelled':[]}))
    return binary
