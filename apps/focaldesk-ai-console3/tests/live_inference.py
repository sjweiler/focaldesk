"""Verify local Ollama inference through an isolated real Rust AI service.
The memory store is temporary; the production index and conversations are untouched.
"""
import json,os,pathlib,struct,subprocess,tempfile,time,zlib
repo=pathlib.Path(__file__).resolve().parents[3]
root=pathlib.Path(tempfile.mkdtemp(prefix='focaldesk-console-live-'))
env=os.environ.copy();env.update(FOCALDESK_CONSOLE_FIXTURE_ROOT=str(root),FOCALDESK_CONSOLE_LIVE_MODEL='gemma4:12b',FOCALDESK_AI_SOCKET=str(root/'ai.sock'),FOCALDESK_AI_PERMISSION='allow-session')
def chunk(kind,data):return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',64,64,8,2,0,0,0))+chunk(b'IDAT',zlib.compress((b'\0'+b'\xff\0\0'*64)*64))+chunk(b'IEND',b'')
image=root/'synthetic-red.png';image.write_bytes(png)
with (root/'backend.log').open('wb') as log:
 server=subprocess.Popen([str(repo/'target/debug/examples/console_fixture')],env=env,stdout=log,stderr=subprocess.STDOUT)
 try:
  for _ in range(100):
   if (root/'ai.sock').exists():break
   if server.poll() is not None:raise RuntimeError('probe service exited')
   time.sleep(.05)
  for args in [['--check-stream-stop'],['--check-image',str(image)]]:
   result=subprocess.run([str(repo/'target/debug/focaldesk-ai-console'),*args],env=env,capture_output=True,text=True,timeout=150)
   print(json.dumps({'check':args[0],'exit':result.returncode,'stdout':result.stdout,'stderr':result.stderr}),flush=True)
   if result.returncode:raise RuntimeError('live inference check failed')
 finally:
  server.terminate();server.wait(timeout=10)
print(json.dumps({'result':'passed','model':'gemma4:12b','fixture_root':str(root)}),flush=True)
