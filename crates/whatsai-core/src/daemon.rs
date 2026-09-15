use crate::{client::Client, protocol::*, storage};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, rc::Rc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex, watch},
};

pub async fn request(state: &Path, command: Value) -> Result<Value> {
    let stream = UnixStream::connect(state.join("daemon.sock"))
        .await
        .context("daemon unavailable; start whatsai-daemon for this state directory")?;
    let (mut read, mut write) = stream.into_split();
    let mut bytes = serde_json::to_vec(&command)?;
    bytes.push(b'\n');
    write.write_all(&bytes).await?;
    let mut reply = String::new();
    BufReader::new((&mut read).take(16 * 1024 * 1024))
        .read_line(&mut reply)
        .await?;
    let v: Value = serde_json::from_str(&reply)?;
    ensure!(
        v["ok"] == true,
        "{}",
        v["error"].as_str().unwrap_or("daemon error")
    );
    Ok(v["result"].clone())
}
pub async fn run(state: &Path, name: &str) -> Result<()> {
    tokio::task::LocalSet::new()
        .run_until(run_local(state, name))
        .await
}
async fn run_local(state: &Path, name: &str) -> Result<()> {
    let _lock = storage::lock(state)?;
    let mut initial = Client::open(state, name)?;
    let relay = std::env::var("WHATSAI_RELAY").ok();
    let endpoint = crate::transport::bind(&initial.identity, relay.as_deref()).await?;
    initial.endpoint = Some(endpoint.clone());
    initial.set("endpoint", &serde_json::to_string(&endpoint.addr())?)?;
    let client = Rc::new(Mutex::new(initial));
    let path = state.join("daemon.sock");
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let (stop, mut stopped) = watch::channel(false);
    let sync_client = client.clone();
    let mut sync_stop = stop.subscribe();
    let sync_endpoint = endpoint.clone();
    let sync = tokio::task::spawn_local(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(2));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {_=sync_stop.changed()=>break,_=timer.tick()=>{let mut c=sync_client.lock().await;let result=c.sync().await;let _=c.set("last_sync_error",&result.err().map(|e|e.to_string()).unwrap_or_default());
            let _=c.set("endpoint",&serde_json::to_string(&sync_endpoint.addr()).unwrap_or_default());
            let batch=direct_batch(&c).unwrap_or_default();drop(c);
            for(eid,recipient,address,envelope)in batch {
             if let Ok((reply,path))=crate::transport::exchange(&sync_endpoint,address,&json!({"method":"deliver","envelope":envelope})).await
              && reply["ok"]==true {let c=sync_client.lock().await;let _=c.set(&format!("direct/{eid}/{recipient}"),&path);let _=c.set("last_peer_path",&path);}
            }}}
        }
    });
    let worker_client = client.clone();
    let mut worker_stop = stop.subscribe();
    let worker = tokio::task::spawn_local(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {_=worker_stop.changed()=>break,_=timer.tick()=>{
             let work={let mut c=worker_client.lock().await;if c.config("last_sync_error").ok().flatten().is_some_and(|s|!s.is_empty()){None}else{c.claim_work().ok().flatten()}};
             if let Some(work)=work {let result=crate::worker::execute(&work).await;let c=worker_client.lock().await;if let Err(e)=c.finish_work(&work,result){let _=c.set("worker_error",&e.to_string());}}
            }}
        }
    });
    eprintln!("WhatsAI daemon ready: {}", path.display());
    loop {
        tokio::select! {
         _=shutdown_signal()=>{break;},
         _=stopped.changed()=>{break;},
         incoming=endpoint.accept()=>{
          if let Some(incoming)=incoming {
           let client=client.clone();
           tokio::task::spawn_local(async move{
            let _=tokio::time::timeout(Duration::from_secs(15),async{
             let connection=incoming.await?;
             let(mut send,mut recv)=connection.accept_bi().await?;
             let result=async {
              let body=recv.read_to_end(MAX_FRAME).await?;let request:Value=serde_json::from_slice(&body)?;
              if request["method"]=="fetch_chunk" {
               let req:Signed<Request>=serde_json::from_value(request["request"].clone())?;crate::crypto::verify(&req)?;
               ensure!(req.body.version==VERSION && now().abs_diff(req.body.timestamp)<=300,"stale peer request");
               let c=client.lock().await;let team=c.refresh().await?;
               let address:iroh::EndpointAddr=serde_json::from_value(team.endpoints.get(&req.signer).context("unknown peer endpoint")?.clone())?;
               ensure!(address.id==connection.remote_id() && req.body.operation["team"]==team.id,"peer request binding mismatch");
               let file=crate::service::field(&req.body.operation,"file")?;let index=req.body.operation["index"].as_i64().context("missing chunk index")?;ensure!((0..32).contains(&index),"invalid chunk index");
               c.rpc(json!({"method":"authorize","event":file,"recipient":req.signer})).await?;
               let body:String=c.db.query_row("SELECT envelope FROM chunks WHERE file=? AND idx=?",rusqlite::params![file,index],|r|r.get(0))?;
               return Ok::<_,anyhow::Error>(json!({"ok":true,"envelope":serde_json::from_str::<Value>(&body)?}));
              }
              ensure!(request["method"]=="deliver","unknown peer operation");
              let env:Signed<Sealed>=serde_json::from_value(request["envelope"].clone())?;crate::crypto::verify(&env)?;
              let mut c=client.lock().await;let team=c.refresh().await?;
              let addr:iroh::EndpointAddr=serde_json::from_value(team.endpoints.get(&env.signer).context("unknown peer endpoint")?.clone())?;
              ensure!(addr.id==connection.remote_id(),"peer endpoint does not match sender");
              let auth=c.rpc(json!({"method":"authorize","event":env.body.header.id,"recipient":c.identity.member()?.id})).await?;
              ensure!(auth["digest"]==digest(&serde_json::to_vec(&env)?),"envelope does not match stored event");
              c.receive(auth["seq"].as_i64().context("missing sequence")?,&env)?;
              c.rpc(json!({"method":"ack","event":env.body.header.id})).await?;
              Ok::<_,anyhow::Error>(json!({"ok":true}))
             }.await;
             let reply=result.unwrap_or_else(|e|json!({"ok":false,"error":e.to_string()}));send.write_all(&serde_json::to_vec(&reply)?).await?;send.finish()?;let _=connection.closed().await;
             Ok::<_,anyhow::Error>(())
            }).await;
           });
          }
         },
         incoming=listener.accept()=>{
          let(stream,_)=incoming?;let client=client.clone();let stop=stop.clone();
          tokio::task::spawn_local(async move{
           let(read,mut write)=stream.into_split();let mut input=String::new();
           let result=async{
            tokio::time::timeout(Duration::from_secs(10),BufReader::new(read.take(MAX_FRAME as u64+1)).read_line(&mut input)).await??;
            ensure!(input.len()<=MAX_FRAME,"local request too large");let command:Value=serde_json::from_str(&input)?;
            if command["action"]=="stop"{let _=stop.send(true);return Ok(json!({"state":"stopping"}));}
            client.lock().await.command(command).await
           }.await;
           let response=match result{Ok(v)=>json!({"ok":true,"result":v}),Err(e)=>json!({"ok":false,"error":format!("{e:#}")})};
           if let Ok(mut bytes)=serde_json::to_vec(&response){bytes.push(b'\n');let _=write.write_all(&bytes).await;}
          });
         }
        }
    }
    let _ = stop.send(true);
    sync.abort();
    worker.abort();
    let _ = sync.await;
    let _ = worker.await;
    endpoint.close().await;
    std::fs::remove_file(path)?;
    Ok(())
}

fn direct_batch(c: &Client) -> Result<Vec<(String, String, iroh::EndpointAddr, Value)>> {
    let Ok(team) = c.team() else {
        return Ok(vec![]);
    };
    let me = c.identity.member()?.id;
    let rows:Vec<(String,String)>=c.db.prepare("SELECT id,envelope FROM outbox WHERE state='service-stored' ORDER BY rowid DESC LIMIT 20")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let mut out = vec![];
    for (eid, body) in rows {
        for recipient in team.members.keys() {
            if recipient == &me || c.config(&format!("direct/{eid}/{recipient}"))?.is_some() {
                continue;
            }
            if let Some(addr) = team.endpoints.get(recipient) {
                out.push((
                    eid.clone(),
                    recipient.clone(),
                    serde_json::from_value(addr.clone())?,
                    serde_json::from_str(&body)?,
                ));
            }
        }
    }
    Ok(out)
}

pub async fn shutdown_signal() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("register SIGTERM handler");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
}
