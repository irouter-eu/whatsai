#!/usr/bin/env python3
"""Local process-level acceptance: isolated authority and three real daemons."""
import json, os, pathlib, subprocess, tempfile, time, hashlib, socket, sqlite3
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=ROOT/'target/debug'

def main():
 with tempfile.TemporaryDirectory(prefix='whatsai-smoke-') as tmp:
  base=pathlib.Path(tmp); processes=[]
  sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
  def start(name,args):
   log=open(base/(name+'.log'),'w');p=subprocess.Popen(args,stdout=log,stderr=log);processes.append((p,log));return p
  def cli(who,*args,check=True):
   r=subprocess.run([str(BIN/'whatsai'),'--state',str(base/who),*args],capture_output=True,text=True,timeout=60)
   if check and r.returncode:raise AssertionError(r.stderr)
   return json.loads(r.stdout) if r.returncode==0 else r
  def rpc(who,op):
   r=subprocess.run([str(BIN/'whatsai'),'--state',str(base/who),'rpc'],input=json.dumps(op),capture_output=True,text=True,timeout=60)
   if r.returncode:raise AssertionError(r.stderr)
   return json.loads(r.stdout)
  def await_ready(who):
   for _ in range(100):
    r=cli(who,'health',check=False)
    if isinstance(r,dict):return
    time.sleep(.1)
   raise AssertionError('daemon did not start')
  try:
   service=start('service',[str(BIN/'whatsai-service'),'--state',str(base/'service'),'--listen',f'127.0.0.1:{port}'])
   time.sleep(.3)
   nodes={}
   for who in ['alice','bob','charlie']:
    nodes[who]=start(who,[str(BIN/'whatsai-daemon'),'--state',str(base/who),'--name',who]);await_ready(who)
   invitation=cli('alice','create','--service',f'http://127.0.0.1:{port}','--repository','https://example.com/team/repo.git')
   assert cli('bob','join',invitation['join'])['state']=='pending'
   bob=cli('bob','register')['id'];alice=cli('alice','register')['id']
   assert cli('bob','list',check=False).returncode!=0
   cli('alice','approve',bob);cli('bob','join-status');cli('alice','promote',bob)
   cli('alice','stop');nodes['alice'].wait(timeout=5)
   assert cli('charlie','join',invitation['join'])['state']=='pending'
   charlie=cli('charlie','register')['id'];cli('bob','approve',charlie);cli('charlie','join-status')
   print('PASS promoted admin admits while creator is offline',flush=True)
   eid=cli('bob','send','synthetic durable message')['id'];cli('bob','sync')
   cli('bob','stop');nodes['bob'].wait(timeout=5)
   cli('charlie','sync');assert any(x['id']==eid for x in cli('charlie','inbox'))
   nodes['bob']=start('bob-restart',[str(BIN/'whatsai-daemon'),'--state',str(base/'bob'),'--name','bob']);await_ready('bob')
   cli('charlie','sync');assert sum(x['id']==eid for x in cli('charlie','inbox'))==1
   print('PASS durable message with sender offline and deduplicated replay',flush=True)
   content=(b'WhatsAI acceptance file\n'*(32*1024*1024//23+1))[:32*1024*1024]
   fixture=base/'fixture.bin';fixture.write_bytes(content)
   fid=cli('bob','share',str(fixture))['id'];cli('bob','sync');cli('charlie','sync')
   downloads=base/'downloads';downloads.mkdir()
   partial=cli('charlie','download',fid,'--directory',str(downloads),'--max-chunks','16');assert partial['state']=='partial'
   cli('charlie','stop');nodes['charlie'].wait(timeout=5)
   with sqlite3.connect(base/'charlie/client.db') as db:
    db.execute('UPDATE downloads SET bytes=? WHERE file=? AND idx=0',(b'corrupted cached chunk',fid))
   nodes['charlie']=start('charlie-restart',[str(BIN/'whatsai-daemon'),'--state',str(base/'charlie'),'--name','charlie']);await_ready('charlie')
   result=cli('charlie','download',fid,'--directory',str(downloads));assert result['new_chunks']==17
   assert hashlib.sha256((downloads/'fixture.bin').read_bytes()).hexdigest()==hashlib.sha256(content).hexdigest()
   assert cli('charlie','download',fid,'--directory',str(downloads),check=False).returncode!=0
   assert cli('charlie','health')['last_file_path']=='direct'
   print('PASS 32 MiB direct file, restart/resume, corrupted-cache recovery, hash verification, no overwrite',flush=True)
   cli('bob','status','--set','blocked','--description','Waiting for review');cli('bob','sync');cli('charlie','sync')
   assert any(x['member']==bob for x in cli('charlie','status'))
   # A second download keeps an incomplete cache for the revocation gate.
   small=base/'second.bin';small.write_bytes(content[:2*1024*1024]);fid2=cli('bob','share',str(small))['id'];cli('bob','sync');cli('charlie','sync')
   cli('charlie','download',fid2,'--directory',str(downloads),'--max-chunks','1')
   cli('bob','revoke',charlie)
   assert cli('charlie','download',fid2,'--directory',str(downloads),check=False).returncode!=0
   assert cli('charlie','sync',check=False).returncode!=0
   print('PASS revoked member cannot resume file or sync',flush=True)
   assert cli('bob','demote',bob)['admins']==[alice]
   assert cli('bob','promote',bob,check=False).returncode!=0
   print('PASS ordinary member cannot promote itself',flush=True)
   service.terminate();service.wait(timeout=10)
   offline=cli('bob','send','queued during authority outage')['id']
   assert any(x['id']==offline and x['state']=='queued' for x in cli('bob','outbox'))
   service=start('service-restart',[str(BIN/'whatsai-service'),'--state',str(base/'service'),'--listen',f'127.0.0.1:{port}']);time.sleep(.5)
   cli('bob','sync')
   assert any(x['id']==offline and x['state']=='service-stored' for x in cli('bob','outbox'))
   print('PASS authority-outage queueing and recovery',flush=True)
  finally:
   for p,log in processes:
    if p.poll() is None:p.terminate()
   for p,log in processes:
    try:p.wait(timeout=5)
    except subprocess.TimeoutExpired:p.kill();p.wait()
    log.close()
if __name__=='__main__':main()
