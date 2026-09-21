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
  assert.equal(v.adapter,'0.9.3');assert.equal(v.plugin,'test-plugin');assert.match(v.daemon,/^\d+\.\d+\.\d+$/);assert.equal(typeof v.database,'number');assert.equal(v.mismatch,v.daemon!=='0.9.3');
  const teams=await client.callTool({name:'whatsai',arguments:{action:'teams'}});
  assert.match((teams.content as any[])[0].text,/^No teams\./,'an empty table still reads as a sentence');
  const unread=await client.callTool({name:'whatsai',arguments:{action:'unread'}});
  const counts=JSON.parse((unread.content as any[])[0].text);assert.match(counts.agent,/^testharness@/);assert.equal(counts.addressed,0);
 }finally{
  await client.close();daemon.kill('SIGTERM');await new Promise<void>(resolve=>{if(daemon.exitCode!==null)return resolve();daemon.once('exit',()=>resolve());});rmSync(dir,{recursive:true,force:true});
 }
});
