import {local} from './local.js';

export interface Attached {label?:string;lease?:string;published?:boolean;enrolled?:boolean;team?:string;harness:string;workspace:string;}

/**
 * Every call a session makes carries `via`: its agent label, or "unattached" when the daemon
 * never accepted an attach. The daemon refuses team actions unless that agent is enrolled, so
 * a session in an unrelated checkout cannot read, send, or reveal the join key.
 */
export function stamp(command:Record<string,unknown>,session:Attached):Record<string,unknown> {
 return {...command,via:session.label??'unattached'};
}

/** Which coding agent launched us: explicit env first, then the harness's own markers. */
export function detectHarness(env:NodeJS.ProcessEnv=process.env):string {
 const explicit=env.WHATSAI_HARNESS?.trim().toLowerCase();
 if(explicit)return explicit;
 if(env.CLAUDECODE || env.CLAUDE_CODE_SESSION_ID)return 'claude';
 if(env.CODEX_SANDBOX || env.CODEX_HOME || env.CODEX_THREAD_ID)return 'codex';
 return 'agent';
}
export function detectSession(env:NodeJS.ProcessEnv=process.env):string|undefined {
 return env.WHATSAI_SESSION || env.CLAUDE_CODE_SESSION_ID || env.CODEX_THREAD_ID || undefined;
}

/**
 * Register this process as a session of the agent for (harness, cwd), heartbeat while alive, and
 * detach on exit. Never throws: a daemon that cannot start leaves the tool usable as the person.
 */
export async function attachAgent(opts:{binary?:string;cwd?:string;heartbeatMs?:number}={}):Promise<Attached> {
 const harness=detectHarness();const workspace=opts.cwd??process.cwd();
 const attached:Attached={harness,workspace};
 let result:any;
 try{
  result=await local({action:'agent',operation:'attach',harness,workspace,session:detectSession(),pid:process.pid},opts.binary);
 }catch(e){
  console.error(`whatsai-mcp: not attached as an agent (${e instanceof Error?e.message:String(e)})`);
  return attached;
 }
 attached.label=result?.agent?.label;attached.lease=result?.lease;attached.published=result?.agent?.published===true;attached.enrolled=result?.agent?.enrolled===true;attached.team=result?.agent?.team_name??undefined;
 if(!attached.lease)return attached;
 const lease=attached.lease;
 const timer=setInterval(()=>{local({action:'agent',operation:'heartbeat',lease},opts.binary).catch(()=>{});},opts.heartbeatMs??15_000);
 timer.unref();
 let detached=false;
 const detach=()=>{if(detached)return;detached=true;clearInterval(timer);return local({action:'agent',operation:'detach',lease},opts.binary).catch(()=>{});};
 for(const signal of ['SIGTERM','SIGINT','SIGHUP'] as const)process.once(signal,()=>{Promise.resolve(detach()).finally(()=>process.exit(0));});
 process.stdin.once('end',()=>{Promise.resolve(detach()).finally(()=>process.exit(0));});
 process.once('beforeExit',()=>{void detach();});
 return attached;
}
