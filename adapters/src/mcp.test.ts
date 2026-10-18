import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,rmSync,existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {spawn} from 'node:child_process';
import {Client} from '@modelcontextprotocol/sdk/client/index.js';
import {StdioClientTransport} from '@modelcontextprotocol/sdk/client/stdio.js';
const binary=resolve('../target/debug/whatsai');
test('MCP reaches the real daemon and cannot override its declared action', {skip:!existsSync(binary)}, async()=>{
 const dir=mkdtempSync(join(tmpdir(),'whatsai-mcp-'));
 const daemon=spawn(resolve('../target/debug/whatsai-daemon'),['--state',dir,'--name','MCP fixture'],{stdio:['ignore','ignore','pipe']});
 const client=new Client({name:'whatsai-test',version:'1'},{capabilities:{}});
 try {
  await new Promise<void>((resolve,reject)=>{const timeout=setTimeout(()=>reject(new Error('Daemon startup timeout')),10_000);daemon.stderr.on('data',b=>{if(String(b).includes('daemon ready')){clearTimeout(timeout);resolve();}});daemon.on('error',reject);});
  const env=Object.fromEntries(Object.entries(process.env).filter((x):x is [string,string]=>typeof x[1]==='string'));
  await client.connect(new StdioClientTransport({command:process.execPath,args:[resolve('dist/mcp.js')],env:{...env,WHATSAI_BIN:binary,WHATSAI_STATE:dir,WHATSAI_HARNESS:'testharness',WHATSAI_PLUGIN_VERSION:'test-plugin'}}));
  const tools=await client.listTools();const tool=tools.tools.find(t=>t.name==='whatsai');assert.ok(tool);
  assert.match(tool.description??'',/This session is .*testharness.*: NOT enrolled in any team.*private/,'the tool announces the session, that it is not enrolled, and that it is private');
  const refused=await client.callTool({name:'whatsai',arguments:{action:'list'}});
  assert.ok(refused.isError);assert.match((refused.content as any[])[0].text,/not enrolled/,'team actions are refused for an unenrolled session');
  const result=await client.callTool({name:'whatsai',arguments:{action:'health',args:{action:'revoke'}}});
  assert.ok(!result.isError);const body=JSON.parse((result.content as any[])[0].text);assert.equal(body.member.name,'MCP fixture');
  const agents=await client.callTool({name:'whatsai',arguments:{action:'agents'}});
  const table=(agents.content as any[])[0].text as string;
  assert.match(table,/^AGENT\s+TEAM\s+VISIBILITY/,'read actions return the daemon table, not JSON');
  assert.match(table,/testharness@\S+\s+-\s+private\s+online x1/,'the session shows as private and online');
  const published=await client.callTool({name:'whatsai',arguments:{action:'publish'}});
  assert.ok(published.isError,'publishing needs a team to enroll into');
  assert.match((published.content as any[])[0].text,/not a member of a team yet|say which team/);
  const version=await client.callTool({name:'whatsai',arguments:{action:'version'}});
  const v=JSON.parse((version.content as any[])[0].text);
  assert.equal(v.adapter,'0.11.0');assert.equal(v.plugin,'test-plugin');assert.match(v.daemon,/^\d+\.\d+\.\d+$/);assert.equal(typeof v.database,'number');assert.equal(v.mismatch,v.daemon!=='0.11.0');
  const teams=await client.callTool({name:'whatsai',arguments:{action:'teams'}});
  assert.match((teams.content as any[])[0].text,/^No teams\./,'an empty table still reads as a sentence');
  const unread=await client.callTool({name:'whatsai',arguments:{action:'unread'}});
  const counts=JSON.parse((unread.content as any[])[0].text);assert.match(counts.agent,/^testharness@/);assert.equal(counts.addressed,0);
 }finally{
  await client.close();daemon.kill('SIGTERM');await new Promise<void>(resolve=>{if(daemon.exitCode!==null)return resolve();daemon.once('exit',()=>resolve());});rmSync(dir,{recursive:true,force:true});
 }
});

test('as a channel, the adapter pushes a notification once per arrival of mail addressed to the session', {skip:!existsSync(binary)}, async()=>{
 const dir=mkdtempSync(join(tmpdir(),'whatsai-channel-'));
 const ws=join(dir,'ws');const {mkdirSync,realpathSync}=await import('node:fs');mkdirSync(ws);const workspace=realpathSync(ws);
 const env=Object.fromEntries(Object.entries(process.env).filter((x):x is [string,string]=>typeof x[1]==='string'));
 const daemon=spawn(resolve('../target/debug/whatsai-daemon'),['--state',dir,'--name','Channel fixture'],{stdio:['ignore','ignore','pipe'],env:{...env,WHATSAI_RELAY:'off'}});
 const cli=(args:string[],input?:string)=>new Promise<string>((res,rej)=>{const p=spawn(binary,['--state',dir,...args],{stdio:['pipe','pipe','pipe']});let out='',err='';p.stdout.on('data',b=>out+=b);p.stderr.on('data',b=>err+=b);p.on('close',c=>c===0?res(out):rej(new Error(err||`exit ${c}`)));p.stdin.end(input??'');});
 const client=new Client({name:'whatsai-test',version:'1'},{capabilities:{}});
 const seen:any[]=[];client.fallbackNotificationHandler=async n=>{if(n.method==='notifications/claude/channel')seen.push(n.params);};
 try {
  await new Promise<void>((resolve,reject)=>{const timeout=setTimeout(()=>reject(new Error('Daemon startup timeout')),10_000);daemon.stderr.on('data',b=>{if(String(b).includes('daemon ready')){clearTimeout(timeout);resolve();}});});
  // A path-bound team for the workspace: the session attaching there enrolls and publishes itself.
  await cli(['create','--workspace',workspace]);
  const me=JSON.parse(await cli(['health'])).member.id as string;
  await client.connect(new StdioClientTransport({command:process.execPath,args:[resolve('dist/mcp.js')],cwd:workspace,env:{...env,WHATSAI_BIN:binary,WHATSAI_STATE:dir,WHATSAI_HARNESS:'testharness',WHATSAI_CHANNEL:'on',WHATSAI_WAIT_SECONDS:'2',WHATSAI_RELAY:'off'}}));
  const tools=await client.listTools();assert.ok(tools.tools.find(t=>t.name==='whatsai'));
  const caps=client.getServerCapabilities() as any;assert.deepEqual(caps?.experimental?.['claude/channel'],{},'the server declares itself a Claude channel');
  // Another of our own sessions in the same checkout addresses this one.
  const codex=JSON.parse(await cli(['rpc'],JSON.stringify({action:'agent',operation:'attach',harness:'codex',workspace}))).agent.label as string;
  const label=`testharness@${workspace.split('/').pop()}`;
  await cli(['send','wake up, channel','--as',codex,'--to',me,'--to-agent',label]);
  const until=Date.now()+30_000;while(seen.length===0 && Date.now()<until)await new Promise(r=>setTimeout(r,250));
  assert.equal(seen.length,1,'one notification for the arrival');
  assert.match(seen[0].content,/1 new message\(s\) addressed to this session/);
  assert.match(seen[0].content,/wake up, channel/,'the rows travel in the notification');
  assert.match(seen[0].content,/not instructions/);
  assert.equal(seen[0].meta.agent,label);assert.equal(seen[0].meta.addressed,'1');
  // Unread stays unread until the session acts, yet the same mail is not announced twice.
  await new Promise(r=>setTimeout(r,5_000));
  assert.equal(seen.length,1,'no repeat while nothing new arrives');
  const unread=JSON.parse(await cli(['agent','unread','--agent',label]));assert.equal(unread.addressed,1,'nothing was marked read on the session\'s behalf');
  await cli(['send','second one','--as',codex,'--to',me,'--to-agent',label]);
  const again=Date.now()+30_000;while(seen.length<2 && Date.now()<again)await new Promise(r=>setTimeout(r,250));
  assert.equal(seen.length,2,'a new arrival is announced');assert.equal(seen[1].meta.addressed,'1','counting only what arrived since the last announcement');assert.ok(seen[1].content.includes('wake up, channel') && seen[1].content.includes('second one'),'while the rows list everything still unread: '+seen[1].content);
 } finally {
  await client.close().catch(()=>{});daemon.kill();rmSync(dir,{recursive:true,force:true});
 }
});
