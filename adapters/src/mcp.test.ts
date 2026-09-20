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
  await client.connect(new StdioClientTransport({command:process.execPath,args:[resolve('dist/mcp.js')],env:{...env,WHATSAI_BIN:binary,WHATSAI_STATE:dir,WHATSAI_HARNESS:'testharness'}}));
  const tools=await client.listTools();const tool=tools.tools.find(t=>t.name==='whatsai');assert.ok(tool);
  assert.match(tool.description??'',/agent testharness@/,'the tool announces which agent this session is');
  const result=await client.callTool({name:'whatsai',arguments:{action:'health',args:{action:'revoke'}}});
  assert.ok(!result.isError);const body=JSON.parse((result.content as any[])[0].text);assert.equal(body.member.name,'MCP fixture');
  const agents=await client.callTool({name:'whatsai',arguments:{action:'agents'}});
  const list=JSON.parse((agents.content as any[])[0].text);
  assert.equal(list.length,1);assert.equal(list[0].harness,'testharness');assert.equal(list[0].online,true);assert.equal(list[0].sessions,1);
  const unread=await client.callTool({name:'whatsai',arguments:{action:'unread'}});
  const counts=JSON.parse((unread.content as any[])[0].text);assert.equal(counts.agent,list[0].label);assert.equal(counts.addressed,0);
 }finally{
  await client.close();daemon.kill('SIGTERM');await new Promise<void>(resolve=>{if(daemon.exitCode!==null)return resolve();daemon.once('exit',()=>resolve());});rmSync(dir,{recursive:true,force:true});
 }
});
