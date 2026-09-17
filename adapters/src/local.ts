import {spawn} from 'node:child_process';
import {userInfo} from 'node:os';

const UNAVAILABLE=/daemon unavailable/;
const RESPONSE_LIMIT=16*1024*1024;
interface Run {code:number|null;stdout:string;stderr:string;}

function run(binary:string,args:string[],input:string|undefined,timeoutMs:number):Promise<Run> {
 return new Promise((resolve,reject)=>{
  const child=spawn(binary,args,{stdio:['pipe','pipe','pipe']});let stdout='',stderr='';
  const timer=setTimeout(()=>{child.kill();reject(new Error(`whatsai ${args[0]} timed out`));},timeoutMs);
  child.stdout.on('data',b=>{stdout+=b;if(stdout.length>RESPONSE_LIMIT){child.kill();reject(new Error('Response exceeds limit'));}});
  child.stderr.on('data',b=>{stderr=(stderr+b).slice(-8192);});
  child.on('error',e=>{clearTimeout(timer);reject(e);});
  child.on('close',code=>{clearTimeout(timer);resolve({code,stdout,stderr});});
  if(input===undefined)child.stdin.end();else child.stdin.end(input);
 });
}
function parse(r:Run,label:string):unknown {
 if(r.code!==0)throw new Error(r.stderr.trim()||`${label} exited ${r.code}`);
 return JSON.parse(r.stdout);
}

/** Identity name for a first launch: WHATSAI_NAME, then the OS account, then the CLI default. */
export function defaultName():string|undefined {
 const configured=process.env.WHATSAI_NAME?.trim();
 if(configured)return configured;
 try{const user=userInfo().username.trim();if(user)return user;}catch{}
 return undefined;
}

let starting:Promise<void>|undefined;
/** Start the daemon for WHATSAI_STATE if needed. Concurrent callers share one attempt. */
export function ensureDaemon(binary=process.env.WHATSAI_BIN ?? 'whatsai'):Promise<void> {
 if(!starting){
  const name=defaultName();
  starting=run(binary,name?['start','--name',name]:['start'],undefined,30_000)
   .then(r=>{if(r.code!==0)throw new Error(r.stderr.trim()||`whatsai start exited ${r.code}`);})
   .finally(()=>{starting=undefined;});
 }
 return starting;
}

/** Send one structured request to the local daemon, starting it on first use. */
export async function local(command: Record<string,unknown>, binary=process.env.WHATSAI_BIN ?? 'whatsai'):Promise<unknown> {
 const input=JSON.stringify(command);
 const first=await run(binary,['rpc'],input,120_000);
 if(first.code===0 || !UNAVAILABLE.test(first.stderr))return parse(first,'CLI');
 await ensureDaemon(binary);
 return parse(await run(binary,['rpc'],input,120_000),'CLI');
}
