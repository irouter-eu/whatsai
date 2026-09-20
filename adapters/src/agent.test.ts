import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,writeFileSync,chmodSync,readFileSync,rmSync,existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {attachAgent,detectHarness,detectSession} from './agent.js';

test('harness detection prefers the explicit variable, then harness markers',()=>{
 assert.equal(detectHarness({WHATSAI_HARNESS:' Claude '}),'claude');
 assert.equal(detectHarness({CLAUDECODE:'1'}),'claude');
 assert.equal(detectHarness({CODEX_HOME:'/x'}),'codex');
 assert.equal(detectHarness({}),'agent');
 assert.equal(detectSession({CLAUDE_CODE_SESSION_ID:'abc'}),'abc');
 assert.equal(detectSession({}),undefined);
});

test('attach registers the session, heartbeats, and survives a missing daemon',async()=>{
 const dir=mkdtempSync(join(tmpdir(),'whatsai-agent-'));
 const binary=join(dir,'whatsai');
 writeFileSync(binary,`#!/bin/sh
input=$(cat)
printf '%s\\n' "$input" >> "${dir}/calls"
case "$input" in
 *'"operation":"attach"'*) echo '{"agent":{"label":"claude@repo"},"lease":"lease-1"}';;
 *'"operation":"heartbeat"'*) echo '{"state":"alive"}';;
 *) echo '{}';;
esac
`);
 chmodSync(binary,0o755);
 try{
  const attached=await attachAgent({binary,cwd:dir,heartbeatMs:50});
  assert.equal(attached.label,'claude@repo');
  assert.equal(attached.lease,'lease-1');
  await new Promise(r=>setTimeout(r,180));
  const calls=readFileSync(join(dir,'calls'),'utf8').trim().split('\n');
  assert.match(calls[0],/"operation":"attach"/);
  assert.ok(calls[0].includes('"workspace":"'+dir+'"'),calls[0]);
  assert.match(calls[0],/"pid":\d+/);
  assert.ok(calls.filter(c=>c.includes('"heartbeat"')).length>=2,'heartbeats keep the lease alive');
 }finally{rmSync(dir,{recursive:true,force:true});}
 const nobody=await attachAgent({binary:'/nonexistent/whatsai',cwd:tmpdir()});
 assert.equal(nobody.label,undefined,'no daemon means no agent, but no crash');
 assert.ok(!existsSync('/nonexistent'));
});
