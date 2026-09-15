import {spawn} from 'node:child_process';
export function local(command: Record<string,unknown>, binary=process.env.WHATSAI_BIN ?? 'whatsai'):Promise<unknown> {
 return new Promise((resolve,reject)=>{
  const child=spawn(binary,['rpc'],{stdio:['pipe','pipe','pipe']});let output='',error='';
  const timer=setTimeout(()=>{child.kill();reject(new Error('WhatsAI daemon request timed out'));},120_000);
  child.stdout.on('data',b=>{output+=b;if(output.length>16*1024*1024){child.kill();reject(new Error('Response exceeds limit'));}});
  child.stderr.on('data',b=>{error=(error+b).slice(-8192);});
  child.on('error',e=>{clearTimeout(timer);reject(e);});
  child.on('close',code=>{clearTimeout(timer);if(code!==0)return reject(new Error(error||`CLI exited ${code}`));try{resolve(JSON.parse(output));}catch(e){reject(e);}});
  child.stdin.end(JSON.stringify(command));
 });
}
