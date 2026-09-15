#!/usr/bin/env python3
"""Render a user-service definition without modifying the user's service manager."""
import argparse,pathlib,plistlib,shlex
p=argparse.ArgumentParser();p.add_argument('--platform',choices=['linux','macos'],required=True);p.add_argument('--binary',type=pathlib.Path,required=True);p.add_argument('--state',type=pathlib.Path,required=True);p.add_argument('--name',default='Member');p.add_argument('--output',type=pathlib.Path,required=True);a=p.parse_args()
args=[str(a.binary.resolve()),'--state',str(a.state.resolve()),'--name',a.name]
if a.platform=='linux':
 def quote(v):return '"'+v.replace('\\','\\\\').replace('"','\\"').replace('%','%%').replace('\n','\\n')+'"'
 text='[Unit]\nDescription=WhatsAI local team daemon\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart='+ ' '.join(map(quote,args))+'\nRestart=on-failure\nRestartSec=3\nUMask=0077\n\n[Install]\nWantedBy=default.target\n'
 a.output.write_text(text)
else:
 a.output.write_bytes(plistlib.dumps({'Label':'local.whatsai.daemon','ProgramArguments':args,'KeepAlive':True,'RunAtLoad':True,'Umask':63}))
print(a.output)
