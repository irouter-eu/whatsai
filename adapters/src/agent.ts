import {local} from './local.js';

export interface Attached {label?:string;lease?:string;published?:boolean;enrolled?:boolean;team?:string;handle?:string;suggested?:string;harness:string;workspace:string;}

/**
 * Every call a session makes carries `via`: its agent label, or "unattached" when the daemon
 * never accepted an attach. The daemon refuses team actions unless that agent is enrolled, so
 * a session in an unrelated checkout cannot read, send, or reveal the join key.
 */
export function stamp(command:Record<string,unknown>,session:Attached):Record<string,unknown> {
 return {...command,via:session.label??'unattached'};
}

/**
 * Which coding agent launched us. An explicit WHATSAI_HARNESS wins; otherwise the parent
 * process decides, because environment markers like CLAUDECODE are inherited by anything
 * started from a Claude Code shell, Codex included. Env markers are only the last resort.
 */
export function detectHarness(env:NodeJS.ProcessEnv=process.env,parent:string=parentCommand()):string {
 const explicit=env.WHATSAI_HARNESS?.trim().toLowerCase();
 if(explicit)return explicit;
 const fromParent=harnessFromCommand(parent);
 if(fromParent)return fromParent;
 if(env.CODEX_SANDBOX || env.CODEX_HOME || env.CODEX_THREAD_ID)return 'codex';
 if(env.CLAUDECODE || env.CLAUDE_CODE_SESSION_ID)return 'claude';
 return 'agent';
}
/** The harness named by a parent process command line, if any. */
export function harnessFromCommand(command:string):string|undefined {
 for(const line of command.split('\n')){
  const first=(line.trim().split(/\s+/)[0]??'').split('/').pop()??'';
  const lower=line.toLowerCase();
  if(first==='codex' || /(^|\/)codex(\s|$)/.test(lower) || /codex-cli|codex\.js/.test(lower))return 'codex';
  if(first==='claude' || /(^|\/)claude(\s|$)/.test(lower) || /claude-code|@anthropic-ai\/claude-code/.test(lower))return 'claude';
 }
 return undefined;
}
/**
 * The command lines of our ancestors, nearest first, up to four levels, joined by newlines:
 * a harness that launches MCP servers through a shell still shows up one step further out.
 * /proc on Linux, ps elsewhere; empty when neither is available.
 */
export function parentCommand():string {
 const lines:string[]=[];
 let pid=process.ppid;
 for(let depth=0;depth<4 && pid>1;depth++){
  let command='';let next=0;
  try{
   const {readFileSync}=require('node:fs');
   command=readFileSync(`/proc/${pid}/cmdline`).toString('utf8').split('\0').join(' ').trim();
   const stat=readFileSync(`/proc/${pid}/stat`,'utf8');
   next=Number(stat.slice(stat.lastIndexOf(')')+2).split(' ')[1]);
  }catch{
   try{
    const {execFileSync}=require('node:child_process');
    const out=execFileSync('ps',['-o','ppid=,command=','-p',String(pid)],{encoding:'utf8'}).trim();
    const m=/^(\d+)\s+(.*)$/.exec(out);
    if(m){next=Number(m[1]);command=m[2];}
   }catch{break;}
  }
  if(command)lines.push(command);
  if(harnessFromCommand(command))break;
  pid=next;
 }
 return lines.join('\n');
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
 attached.label=result?.agent?.label;attached.lease=result?.lease;attached.published=result?.agent?.published===true;attached.enrolled=result?.agent?.enrolled===true;attached.team=result?.agent?.team_name??undefined;attached.handle=result?.agent?.handle??undefined;attached.suggested=result?.agent?.suggested_handle??undefined;
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
