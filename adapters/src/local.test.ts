import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,writeFileSync,chmodSync,existsSync,readFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {local,ensureDaemon,defaultName} from './local.js';
import {runWorker} from './worker.js';

/** A stand-in CLI: `rpc` fails as "daemon unavailable" until `start` has run; every start is counted. */
function fakeCli(startExit=0):{dir:string;binary:string} {
 const dir=mkdtempSync(join(tmpdir(),'whatsai-fake-cli-'));
 const binary=join(dir,'whatsai');
 writeFileSync(binary,`#!/bin/sh
cat >/dev/null
case "$1" in
 start) printf '%s\n' "$*" >> "${dir}/starts"; sleep 0.2; [ ${startExit} -eq 0 ] || { echo "Error: daemon exited" >&2; exit ${startExit}; }; echo '{"member":{"id":"fake"}}';;
 rpc) if [ -f "${dir}/starts" ]; then echo '{"ok":true}'; else echo "Error: daemon unavailable; start whatsai-daemon for this state directory" >&2; exit 1; fi;;
 *) echo "unknown" >&2; exit 2;;
esac
`);
 chmodSync(binary,0o755);
 return {dir,binary};
}

test('missing CLI fails without shell execution',async()=>{await assert.rejects(local({action:'inbox'},'/nonexistent/whatsai'),/ENOENT/);});
test('unknown harness is rejected before execution',async()=>{await assert.rejects(runWorker({harness:'other' as any,cwd:'.',prompt:'test'}),/Unsupported/);});

test('an unavailable daemon is started once and the request retried',async()=>{
 const {dir,binary}=fakeCli();
 try{
  const results=await Promise.all([local({action:'health'},binary),local({action:'inbox'},binary),local({action:'list'},binary)]);
  assert.deepEqual(results,[{ok:true},{ok:true},{ok:true}]);
  const starts=readFileSync(join(dir,'starts'),'utf8').trim().split('\n');
  assert.equal(starts.length,1,'concurrent callers must share one start');
  assert.match(starts[0],/^start --name \S/);
 }finally{rmSync(dir,{recursive:true,force:true});}
});

test('a failed start surfaces the CLI error instead of retrying forever',async()=>{
 const {dir,binary}=fakeCli(1);
 try{
  await assert.rejects(local({action:'health'},binary),/daemon exited/);
  await assert.rejects(ensureDaemon(binary),/daemon exited/);
  assert.equal(readFileSync(join(dir,'starts'),'utf8').trim().split('\n').length,2,'each new attempt after a failure runs start again');
 }finally{rmSync(dir,{recursive:true,force:true});}
});

test('other CLI errors do not trigger a daemon start',async()=>{
 const dir=mkdtempSync(join(tmpdir(),'whatsai-fake-cli-'));
 const binary=join(dir,'whatsai');
 writeFileSync(binary,'#!/bin/sh\ncat >/dev/null\n[ "$1" = start ] && touch "'+dir+'/started"\necho "Error: unknown local operation" >&2\nexit 1\n');
 chmodSync(binary,0o755);
 try{
  await assert.rejects(local({action:'bogus'},binary),/unknown local operation/);
  assert.ok(!existsSync(join(dir,'started')));
 }finally{rmSync(dir,{recursive:true,force:true});}
});

test('the first-launch name prefers WHATSAI_NAME',()=>{
 const previous=process.env.WHATSAI_NAME;
 process.env.WHATSAI_NAME='  Ada  ';
 try{assert.equal(defaultName(),'Ada');}finally{if(previous===undefined)delete process.env.WHATSAI_NAME;else process.env.WHATSAI_NAME=previous;}
});
