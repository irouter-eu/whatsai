#!/usr/bin/env python3
import json,pathlib,subprocess,tempfile,time,plistlib
root=pathlib.Path(__file__).resolve().parents[1];binary=root/'target/debug/whatsai'
with tempfile.TemporaryDirectory(prefix='whatsai-lifecycle-') as d:
 def call(*args):return json.loads(subprocess.check_output([str(binary),'--state',d,*args],text=True))
 a=call('start','--name','Lifecycle')['member']['id'];assert call('start')['member']['id']==a
 call('stop')
 for _ in range(50):
  if not pathlib.Path(d,'daemon.sock').exists():break
  time.sleep(.1)
 assert call('start')['member']['id']==a
 call('stop')
 for _ in range(50):
  if not pathlib.Path(d,'daemon.sock').exists():break
  time.sleep(.1)
 assert not pathlib.Path(d,'daemon.sock').exists()
print('PASS CLI daemon start/stop/restart preserves identity')
