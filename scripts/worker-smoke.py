#!/usr/bin/env python3
"""Opt-in live model smoke: isolated daemons; existing harness login; no tool use."""
import json,pathlib,subprocess,tempfile,time,socket,os,shlex
ROOT=pathlib.Path(__file__).resolve().parents[1];BIN=ROOT/'target/debug'
def main():
 with tempfile.TemporaryDirectory(prefix='whatsai-workers-') as tmp:
  base=pathlib.Path(tmp);procs=[];s=socket.socket();s.bind(('127.0.0.1',0));port=s.getsockname()[1];s.close()
  def start(name,args):
   log=open(base/(name+'.log'),'w');p=subprocess.Popen(args,stdout=log,stderr=log);procs.append((p,log));return p
  def cli(who,*args):
   r=subprocess.run([str(BIN/'whatsai'),'--state',str(base/who),*args],capture_output=True,text=True,timeout=60)
   if r.returncode:raise RuntimeError(r.stderr)
   return json.loads(r.stdout)
  try:
   start('service',[str(BIN/'whatsai-service'),'--state',str(base/'service'),'--listen',f'127.0.0.1:{port}']);time.sleep(.4)
   for who in ['sender','codex','claude']:
    start(who,[str(BIN/'whatsai-daemon'),'--state',str(base/who),'--name',who]);time.sleep(.4)
   repo=base/'repo';repo.mkdir();bare=base/'repo.git'
   def git(where,*args):return subprocess.check_output(['git','-C',str(where),*args],stderr=subprocess.DEVNULL,text=True).strip()
   git(repo,'init','-q');git(repo,'config','user.email','test@example.invalid');git(repo,'config','user.name','Synthetic Test')
   (repo/'double.py').write_text('def double(value):\n    return value * 2\n')
   git(repo,'add','double.py');git(repo,'commit','-qm','review fixture');commit=git(repo,'rev-parse','HEAD');git(repo,'clone','--bare','.',str(bare))
   ssh=base/'fixture-ssh';ssh.write_text('#!/bin/sh\nexec git-upload-pack '+shlex.quote(str(bare))+'\n');ssh.chmod(0o700)
   git(repo,'remote','add','origin','git@localhost:fixture.git');git(repo,'config','core.sshCommand',str(ssh));git(repo,'config','ssh.variant','simple')
   card=cli('sender','create','--service',f'http://127.0.0.1:{port}','--repository','git@localhost:fixture.git')['join']
   ids={}
   for who in ['codex','claude']:
    cli(who,'join',card);ids[who]=cli(who,'register')['id'];cli('sender','approve',ids[who]);cli(who,'join-status')
    cwd=base/(who+'-workspace');cwd.mkdir()
    cli(who,'worker','bind','--harness',who,'--cwd',str(cwd),'--adapter',str(ROOT/'adapters/dist/worker.js'))
   for who in ['codex','claude']:
    cli('sender','sync');marker=f'WHATSAI_{who.upper()}_ACK'
    eid=cli('sender','send',f'Reply with exactly {marker}. Do not call any tools.','--to',ids[who])['id'];cli('sender','sync');cli(who,'sync')
    time.sleep(1);assert not cli(who,'worker','status')['binding']['enabled']
    cli(who,'worker','enable');t=time.monotonic();reply=None
    for _ in range(75):
     time.sleep(2);cli('sender','sync')
     found=[v for v in cli('sender','inbox') if v['event'].get('reply_to')==eid]
     if found:reply=found[0];break
     status=cli(who,'worker','status')
     if status.get('last_error'):raise RuntimeError(status['last_error'])
    assert reply and marker in reply['event']['text'],f'no correct reply from {who}'
    cli(who,'worker','pause')
    print(json.dumps({'harness':who,'success':True,'response_ms':round((time.monotonic()-t)*1000),'actor':reply['event']['actor']}),flush=True)
    cli('sender','status','--set','ready','--description','Review fixture ready')
    handoff=cli('sender','handoff','--branch','main','--commit',commit,'--description','Review double.py')['id'];cli('sender','sync');cli(who,'sync')
    review=base/(who+'-review');accepted=cli(who,'accept-handoff',handoff,'--repo',str(repo),'--directory',str(review));assert accepted['commit']==commit
    source=(review/'double.py').read_text();cli(who,'worker','enable')
    question=f'Review this exact committed source from {commit}:\n{source}\nIf double(4) returns 8, reply REVIEW_ACK. Otherwise explain the bug. Do not call tools.'
    review_id=cli('sender','send',question,'--to',ids[who])['id'];cli('sender','sync')
    reviewed=None
    for _ in range(75):
     time.sleep(2);cli('sender','sync');found=[v for v in cli('sender','inbox') if v['event'].get('reply_to')==review_id]
     if found:reviewed=found[0];break
     status=cli(who,'worker','status')
     if status.get('last_error'):raise RuntimeError(status['last_error'])
    assert reviewed and 'REVIEW_ACK' in reviewed['event']['text']
    cli(who,'worker','pause');assert cli(who,'status')
    print(json.dumps({'harness':who,'case':'git_handoff_review','success':True,'commit':commit}),flush=True)
  finally:
   for p,log in procs:
    if p.poll() is None:p.terminate()
   for p,log in procs:
    try:p.wait(timeout=10)
    except subprocess.TimeoutExpired:p.kill();p.wait()
    log.close()
if __name__=='__main__':main()
