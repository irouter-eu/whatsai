#!/usr/bin/env python3
"""Local process-level acceptance: three real daemons; the founder hosts the network authority.

Runs with WHATSAI_RELAY=off so it is hermetic; relay traversal is covered by the Rust transport tests."""
import json, os, pathlib, subprocess, tempfile, time, hashlib, sqlite3
ROOT=pathlib.Path(__file__).resolve().parents[1]
BIN=ROOT/'target/debug'

def main():
 with tempfile.TemporaryDirectory(prefix='whatsai-smoke-') as tmp:
  base=pathlib.Path(tmp); processes=[]
  env={**os.environ,'WHATSAI_RELAY':'off'}
  def start(name,args):
   log=open(base/(name+'.log'),'w');p=subprocess.Popen(args,stdout=log,stderr=log,env=env);processes.append((p,log));return p
  def cli(who,*args,check=True):
   r=subprocess.run([str(BIN/'whatsai'),'--state',str(base/who),*args],capture_output=True,text=True,timeout=60)
   if check and r.returncode:raise AssertionError(r.stderr)
   return json.loads(r.stdout) if r.returncode==0 else r
  def rpc(who,op):
   r=subprocess.run([str(BIN/'whatsai'),'--state',str(base/who),'rpc'],input=json.dumps(op),capture_output=True,text=True,timeout=60)
   if r.returncode:raise AssertionError(r.stderr)
   return json.loads(r.stdout)
  def rpc_ok(who,op):
   return subprocess.run([str(BIN/'whatsai'),'--state',str(base/who),'rpc'],input=json.dumps(op),capture_output=True,text=True,timeout=60).returncode==0
  def await_ready(who):
   for _ in range(100):
    r=cli(who,'health',check=False)
    if isinstance(r,dict):return
    time.sleep(.1)
   raise AssertionError('daemon did not start')
  try:
   nodes={}
   for who in ['alice','bob','charlie']:
    nodes[who]=start(who,[str(BIN/'whatsai-daemon'),'--state',str(base/who),'--name',who]);await_ready(who)
   invitation=cli('alice','create','--repository','https://example.com/team/repo.git')
   assert invitation['join'].startswith('whatsai1.') and invitation['workspace']=='whatsai'
   assert cli('alice','health')['teams'][0]['role']=='admin' and cli('alice','teams')[0]['founder']==cli('alice','register')['id']
   tampered=invitation['join'][:-6]+'AAAAAA'
   assert cli('bob','join',tampered,check=False).returncode!=0
   assert cli('bob','join',invitation['join'])['state']=='pending'
   bob=cli('bob','register')['id'];alice=cli('alice','register')['id']
   assert cli('bob','list',check=False).returncode!=0
   cli('alice','approve',bob);cli('bob','join-status');cli('alice','promote',bob)
   print('PASS founder-hosted authority: key admits only with approval; tampered keys are refused',flush=True)
   cli('alice','stop');nodes['alice'].wait(timeout=5)
   assert cli('charlie','join',invitation['join'],check=False).returncode!=0
   nodes['alice']=start('alice-restart',[str(BIN/'whatsai-daemon'),'--state',str(base/'alice'),'--name','alice']);await_ready('alice')
   assert cli('charlie','join',invitation['join'])['state']=='pending'
   charlie=cli('charlie','register')['id'];cli('bob','approve',charlie);cli('charlie','join-status')
   print('PASS admissions wait while the founder is offline; a promoted admin admits once it is back',flush=True)
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
   # Agents: durable per harness and checkout, addressable by label, with per-agent unread cursors.
   ws=base/'bob-work/app';ws.mkdir(parents=True)
   attached=rpc('bob',{'action':'agent','operation':'attach','harness':'claude','workspace':str(ws),'session':'s1','pid':1})
   claude_label=attached['agent']['label'];assert claude_label=='claude@app',claude_label
   assert rpc('bob',{'action':'agent','operation':'attach','harness':'codex','workspace':str(ws)})['agent']['label']=='codex@app'
   cli('bob','sync');cli('charlie','sync')
   assert not cli('charlie','list')['agents'].get(bob),'attaching publishes nothing to the team'
   # These checkouts are not the team repository, so their sessions are shut out until enrolled.
   assert not rpc_ok('bob',{'action':'list','via':claude_label}) and not rpc_ok('bob',{'action':'invite','via':'unattached'}),'unenrolled sessions are refused team actions'
   cli('bob','agent','enroll',claude_label);cli('bob','agent','enroll','codex@app')
   assert rpc('bob',{'action':'list','via':claude_label})['id']==cli('bob','teams')[0]['id'],'enrolled sessions get through'
   cli('bob','agent','publish',claude_label);cli('bob','agent','publish','codex@app');cli('bob','sync');cli('charlie','sync')
   published=cli('charlie','list')['agents'][bob];assert {a['label'] for a in published}=={'claude@app','codex@app'} and all('workspace' in a and '/' not in a['workspace'] for a in published)
   assert cli('charlie','send','wrong label','--to',bob,'--to-agent','claude@nowhere',check=False).returncode!=0
   cli('charlie','send','for claude only','--to',claude_label);cli('charlie','send','for everyone');cli('charlie','send','by name','--to','bob');assert cli('charlie','send','nobody','--to','zed',check=False).returncode!=0;cli('charlie','sync');cli('bob','sync')
   named=[x for x in cli('bob','inbox') if x['event']['text']=='by name'];assert named and named[0]['event']['to']==bob and named[0]['event'].get('to_agent') is None,'a name resolves to the member'
   claude_unread=cli('bob','agent','unread','--agent',claude_label);codex_unread=cli('bob','agent','unread','--agent','codex@app')
   assert (claude_unread['addressed'],codex_unread['addressed'])==(1,0),(claude_unread,codex_unread)
   assert claude_unread['shared']>=1
   assert all(x['event'].get('to_agent') in (None,claude_label) for x in cli('bob','inbox','--agent',claude_label))
   cli('bob','agent','mark-read',claude_label);assert cli('bob','agent','unread','--agent',claude_label)['addressed']==0
   assert cli('bob','agent','unread','--agent','codex@app')['shared']>=1,'cursors are per agent'
   cli('bob','status','--set','ready','--as',claude_label,'--description','Agent status');cli('bob','sync');cli('charlie','sync')
   assert any(x['member']==bob and x['agent']==claude_label for x in cli('charlie','status'))
   cli('bob','agent','detach',attached['lease']);cli('bob','agent','retire','codex@app');cli('bob','sync');cli('charlie','sync')
   assert [a['label'] for a in cli('charlie','list')['agents'][bob]]==['claude@app'],'retired agents leave the roster; offline ones stay'
   print('PASS agents are durable, addressable by label, published without paths, with per-agent unread and status',flush=True)
   # A second download keeps an incomplete cache for the revocation gate.
   small=base/'second.bin';small.write_bytes(content[:2*1024*1024]);fid2=cli('bob','share',str(small))['id'];cli('bob','sync');cli('charlie','sync')
   cli('charlie','download',fid2,'--directory',str(downloads),'--max-chunks','1')
   cli('bob','revoke',charlie)
   assert cli('charlie','download',fid2,'--directory',str(downloads),check=False).returncode!=0
   revoked=cli('charlie','sync');assert revoked['state']=='partial' and 'denied' in ' '.join(revoked['errors']),revoked
   print('PASS revoked member cannot resume file or sync',flush=True)
   assert cli('bob','demote',bob)['admins']==[alice]
   assert cli('bob','promote',bob,check=False).returncode!=0
   print('PASS ordinary member cannot promote itself',flush=True)
   cli('alice','stop');nodes['alice'].wait(timeout=5)
   offline=cli('bob','send','queued during founder outage')['id']
   assert any(x['id']==offline and x['state']=='queued' for x in cli('bob','outbox'))
   outage=cli('bob','sync');assert outage['state']=='partial' and 'unreachable' in ' '.join(outage['errors']),outage
   nodes['alice']=start('alice-restart-2',[str(BIN/'whatsai-daemon'),'--state',str(base/'alice'),'--name','alice']);await_ready('alice')
   cli('bob','sync')
   assert any(x['id']==offline and x['state']=='service-stored' for x in cli('bob','outbox'))
   print('PASS founder-outage queueing and recovery on the remembered port',flush=True)
   # A second, path-bound team with no Git anywhere: bob founds it from a plain directory,
   # charlie is revoked from the first team but joins this one; commands select teams by directory or --team.
   notes=base/'bob-notes';notes.mkdir()
   key2=cli('bob','create','--workspace',str(notes))
   assert key2['workspace']=='bob-notes' and key2['repository'] is None
   assert len(cli('bob','teams'))==2
   assert cli('bob','send','ambiguous',check=False).returncode!=0,'two teams and an unbound directory need --team'
   charlie_notes=base/'charlie-notes';charlie_notes.mkdir()
   assert cli('charlie','join',key2['join'],'--workspace',str(charlie_notes))['state']=='pending'
   cli('bob','--team','bob-notes','approve',charlie);cli('charlie','sync')
   assert 'bob-notes' in [t['workspace'] for t in cli('charlie','teams')],'charlie is in the notes team'
   cli('bob','sync');cli('bob','--team','bob-notes','send','notes only');cli('bob','--team','whatsai','send','repo only')
   cli('bob','sync');cli('charlie','sync')
   texts=[x['event']['text'] for x in cli('charlie','inbox')]
   assert 'notes only' in texts and 'repo only' not in texts,'messages stay in their team'
   assert cli('charlie','handoff','--branch','main','--commit','a'*40,check=False).returncode!=0,'no repository, no handoffs'
   attached=rpc('charlie',{'action':'agent','operation':'attach','harness':'claude','workspace':str(charlie_notes)})['agent']
   assert attached['enrolled'] and attached['team_name']=='bob-notes','the joined directory enrolls by exact path'
   stray=rpc('charlie',{'action':'agent','operation':'attach','harness':'claude','workspace':str(base/'charlie')})['agent']
   assert not stray['enrolled']
   other=base/'bob-other';other.mkdir()
   rpc('bob',{'action':'agent','operation':'attach','harness':'claude','workspace':str(other)})
   own=cli('bob','join',key2['join'],'--workspace',str(other))
   assert own['state']=='enrolled' and own['agents']==['claude@bob-other'],own
   shown=rpc('bob',{'action':'agent','operation':'show','agent':'claude@bob-other'});assert shown['team_name']=='bob-notes' and shown['published'],shown
   cli('bob','sync');cli('charlie','sync');assert any(a['label']=='claude@bob-other' for a in cli('charlie','--team','bob-notes','list')['agents'][bob]),'a deliberate join is visible to teammates'
   print('PASS a second, path-bound team without Git: directory-scoped enrolment, --team selection, messages stay put',flush=True)
  finally:
   for p,log in processes:
    if p.poll() is None:p.terminate()
   for p,log in processes:
    try:p.wait(timeout=5)
    except subprocess.TimeoutExpired:p.kill();p.wait()
    log.close()
if __name__=='__main__':main()
