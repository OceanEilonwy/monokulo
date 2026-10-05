//! End-to-end queue properties: real SQLite, real HTTP and independently modelled state.
use super::*;
use crate::property_support::{config, runtime, CrashChild, TempFile};
use crate::store::{Db, NewOrder, NewTenant, Store};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse as _,
    routing::post,
    Router,
};
use proptest::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU16, AtomicUsize};

#[derive(Clone)]
struct Captured {
    headers: HeaderMap,
    body: Vec<u8>,
}
#[derive(Default)]
struct Endpoint {
    captured: parking_lot::Mutex<Vec<Captured>>,
    status: AtomicU16,
    slow: parking_lot::Mutex<HashSet<String>>,
    release: tokio::sync::Notify,
    active: AtomicUsize,
    peak: AtomicUsize,
    redirect: parking_lot::Mutex<Option<String>>,
}
struct Server {
    url: String,
    state: Arc<Endpoint>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn receive(
    State(state): State<Arc<Endpoint>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let status = state.status.load(Ordering::SeqCst);
    let active = state.active.fetch_add(1, Ordering::SeqCst) + 1;
    state.peak.fetch_max(active, Ordering::SeqCst);
    let id = headers
        .get("x-monokulo-event-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    state.captured.lock().push(Captured {
        headers,
        body: body.to_vec(),
    });
    let wait = state.release.notified();
    if state.slow.lock().contains(&id) {
        wait.await;
    }
    state.active.fetch_sub(1, Ordering::SeqCst);
    if let Some(url) = state.redirect.lock().as_ref() {
        return (
            StatusCode::from_u16(status).unwrap(),
            [("location", url.clone())],
        )
            .into_response();
    }
    StatusCode::from_u16(state.status.load(Ordering::SeqCst))
        .unwrap()
        .into_response()
}
impl Server {
    async fn new(status: u16) -> Self {
        let state = Arc::new(Endpoint::default());
        state.status.store(status, Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/hook", post(receive))
            .with_state(Arc::clone(&state));
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { url, state, task }
    }
    fn unblock(&self) {
        self.state.slow.lock().clear();
        self.state.release.notify_waiters();
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    id: i64,
    attempts: u32,
    due: i64,
    delivered: Option<i64>,
    gave_up: Option<i64>,
    status: Option<u16>,
    last: Option<i64>,
    error: Option<String>,
}
struct World {
    path: TempFile,
    store: SharedStore,
    db: Db,
    client: WebhookClient,
}
impl World {
    fn new(worker: bool) -> Self {
        let path = TempFile::new();
        let s = Store::open_file(&path.0).unwrap();
        let db = if worker {
            Db::open(&path.0, &s).unwrap()
        } else {
            Db::over_shared(s.into_shared())
        };
        // A distinct observation connection also makes worker commits visible to the oracle.
        let store = Store::open_file(&path.0).unwrap().into_shared();
        let client = WebhookClient::build().unwrap();
        client.set_allow_private(true);
        Self {
            path,
            store,
            db,
            client,
        }
    }
    fn inline(&mut self) {
        self.db = Db::over_shared(Arc::clone(&self.store));
    }
    fn restart(&mut self) {
        let s = Store::open_file(&self.path.0).unwrap();
        self.db = Db::open(&self.path.0, &s).unwrap();
        self.store = s.into_shared();
    }
    fn seed(
        &self,
        url: &str,
        tenants: usize,
        orders: usize,
        events: usize,
        extra: &str,
    ) -> Vec<Vec<Vec<i64>>> {
        let s = self.store.lock();
        (0..tenants).map(|t| {
            let tenant=s.create_tenant(&NewTenant{key_custody_backend:"plain".into(),sealed_key_material:vec![],primary_address:"4x".into(),network:"mainnet".into(),confirmations_required:None,order_expiry_seconds:None},1000).unwrap();
            let webhook=s.create_webhook(&tenant.tenant.id,url,extra,"whsec_property",1000).unwrap();
            (0..orders).map(|o| {
                let order=s.create_order(&NewOrder{idempotency_key:None,confirmations_required_override:None,tenant_id:tenant.tenant.id.clone(),merchant_order_id:None,minor_index:o as u32+1,address:format!("address_{t}_{o}"),xmr_amount_piconero:1,description:None,created_at:1000,expires_at:2000}).unwrap();
                (0..events).map(|e|s.enqueue_webhook_delivery(&webhook.id,&order.id,"order.paid",&serde_json::json!({"event_id":format!("event_{t}_{o}_{e}"),"event":"order.paid","message":"\u{652f}\u{4ed8} \u{1f980}"}).to_string(),1000).unwrap()).collect()
            }).collect()
        }).collect()
    }
    fn rows(&self) -> Vec<Row> {
        let s = self.store.lock();
        let mut stmt=s.conn_for_test().prepare("SELECT id,attempt_count,next_attempt_at_utc,delivered_at_utc,gave_up_at_utc,last_response_status,last_error,last_attempted_at_utc FROM webhook_deliveries ORDER BY id").unwrap();
        stmt.query_map([], |r| {
            Ok(Row {
                id: r.get(0)?,
                attempts: r.get(1)?,
                due: r.get(2)?,
                delivered: r.get(3)?,
                gave_up: r.get(4)?,
                status: r.get(5)?,
                error: r.get(6)?,
                last: r.get(7)?,
            })
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }
    async fn tick(
        &self,
        now: i64,
        max: u32,
        timeout: Duration,
    ) -> Result<usize, crate::store::StoreError> {
        run_delivery_tick_on(&self.db, &self.client, timeout, max, now).await
    }
}
fn verify(c: &Captured, payload: &str, event_id: &str) {
    assert_eq!(c.body, payload.as_bytes());
    for name in [
        "x-monokulo-signature",
        "x-monokulo-event",
        "x-monokulo-event-id",
        "content-type",
    ] {
        assert_eq!(
            c.headers.get_all(name).iter().count(),
            1,
            "duplicate {name}"
        );
    }
    assert_eq!(c.headers["x-monokulo-event"], "order.paid");
    assert_eq!(c.headers["x-monokulo-event-id"], event_id);
    assert_eq!(c.headers["content-type"], "application/json");
    let sig = c.headers["x-monokulo-signature"].to_str().unwrap();
    assert!(crate::webhook_sign::verify_signature(
        "whsec_property",
        &c.body,
        sig,
        shared::time::now_unix()
    ));
    let signed_at: i64 = sig[2..sig.find(',').unwrap()].parse().unwrap();
    assert!(signed_at.abs_diff(shared::time::now_unix()) < 10);
}
const TIMEOUT: Duration = Duration::from_secs(2);

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn retry_histories_keep_wire_identity_and_survive_reopen(failures in 0u32..10,max in 1u32..9,status in prop_oneof![Just(200u16),Just(201),Just(204),Just(299)],text in any::<String>()) {
        runtime().block_on(async {
            let server=Server::new(503).await;let mut w=World::new(true);let id=w.seed(&server.url,1,1,1,"{\"x-merchant\":\"hello\"}")[0][0][0];
            let payload=serde_json::json!({"event_id":"stable","message":text}).to_string();w.store.lock().conn_for_test().execute("UPDATE webhook_deliveries SET payload_json=?1 WHERE id=?2",rusqlite::params![payload,id]).unwrap();
            let mut now=1000;
            for attempt in 0..max {
                if attempt>=failures {server.state.status.store(status,Ordering::SeqCst);}
                assert_eq!(w.tick(now,max,TIMEOUT).await.unwrap(),1);let row=w.rows()[0].clone();assert_eq!(row.attempts,attempt+1);assert_eq!(row.status,Some(if attempt<failures {503}else{status}));
                for capture in server.state.captured.lock().iter() {verify(capture,&payload,"stable");assert_eq!(capture.headers["x-merchant"],"hello");}
                w.restart();assert_eq!(w.rows()[0],row);
                if attempt>=failures {assert_eq!(row.delivered,row.last);assert!(row.last.unwrap()>=now);assert_eq!(row.gave_up,None);assert_eq!(row.error,None);assert_eq!(w.tick(i64::MAX,max,TIMEOUT).await.unwrap(),0);break;}
                if attempt+1==max {assert_eq!(row.gave_up,row.last);assert!(row.last.unwrap()>=now);assert!(row.error.is_some());assert_eq!(w.tick(i64::MAX,max,TIMEOUT).await.unwrap(),0);break;}
                let expected=60*(1i64<<attempt.min(6));assert_eq!(row.due,row.last.unwrap()+expected);assert_eq!(w.tick(row.due-1,max,TIMEOUT).await.unwrap(),0);now=row.due;
            }
        });
    }
    #[test]
    fn stored_extra_headers_cannot_replace_signed_protocol_headers(variant in 0usize..9,value in "[a-zA-Z0-9]{1,32}") {
        runtime().block_on(async {
            let server=Server::new(200).await;let w=World::new(true);
            let name=["X-Monokulo-Signature","x-monokulo-event","X-MONOKULO-EVENT-ID","Content-Type","HOST","Content-Length","Transfer-Encoding","Connection","X-Monokulo-Other"][variant];
            let extra=serde_json::json!({name:value,"x-merchant":"ok"}).to_string();w.seed(&server.url,1,1,1,&extra);
            let delivery=w.store.lock().due_webhook_deliveries_for_test(1000,1).unwrap().remove(0);
            let result=attempt_delivery(&w.client,&delivery,TIMEOUT).await;assert!(result.delivered,"{:?}",result.error);
            let captures=server.state.captured.lock();assert_eq!(captures.len(),1);verify(&captures[0],&delivery.payload_json,"event_0_0_0");assert_eq!(captures[0].headers["x-merchant"],"ok");
        });
    }
    #[test]
    fn independent_queue_model_enforces_tenant_fairness_and_order_fifo(tenants in 1usize..21,orders in 1usize..9,events in 1usize..5,history in prop::collection::vec((0u8..4,0usize..200),1..25),per_tenant in 0u32..8,limit in 0u32..65) {
        let w=World::new(false);let ids=w.seed("not a url",tenants,orders,events,"{}");
        let mut model:HashMap<i64,(usize,usize,i64,bool)>=HashMap::new();for(t,os)in ids.iter().enumerate(){for(o,es)in os.iter().enumerate(){for id in es {model.insert(*id,(t,o,1000,false));}}}
        for(op,index)in history {
            let id=1+(index%model.len())as i64;let entry=model.get_mut(&id).unwrap();
            match op {0=>{if !entry.3 {w.store.lock().schedule_webhook_retry(id,1100,Some(500),Some("fail"),1000).unwrap();entry.2=1100;}},1=>{w.store.lock().mark_webhook_delivered(id,200,1000).unwrap();entry.3=true;},2=>{w.store.lock().give_up_webhook_delivery(id,Some(500),Some("fail"),1000).unwrap();entry.3=true;},_=>{}}
            for now in [999,1000,1100] {
                let mut heads:HashMap<(usize,usize),(i64,i64)>=HashMap::new();let mut all:Vec<_>=model.iter().collect();all.sort_by_key(|(id,_)|**id);
                for(id,(t,o,due,done))in all {if !done {heads.entry((*t,*o)).or_insert((*id,*due));}}
                let mut eligible:Vec<_>=heads.into_iter().filter(|(_,(_,due))|*due<=now).map(|((t,_),(id,due))|(due,id,t)).collect();eligible.sort_unstable();
                let mut shares=HashMap::new();let expected:Vec<_>=eligible.into_iter().filter(|(_,_,t)|{let count=shares.entry(*t).or_insert(0u32);*count+=1;*count<=per_tenant}).take(limit as usize).map(|(_,id,_)|id).collect();
                let actual:Vec<_>=w.store.lock().due_webhook_deliveries_fair(now,per_tenant,limit).unwrap().into_iter().map(|d|d.delivery_id).collect();assert_eq!(actual,expected);
            }
        }
    }
    #[test]
    fn late_failures_do_not_overwrite_success_or_reopen_terminal_rows(history in prop::collection::vec(0u8..3,1..30),at in 1i64..100_000) {
        let w=World::new(false);w.seed("invalid",1,1,1,"{}");let mut successful=false;let mut terminal=false;
        for op in history {
            let previous=w.rows()[0].clone();match op {0=>{w.store.lock().schedule_webhook_retry(1,at+60,Some(500),Some("late failure"),at).unwrap();},1=>{w.store.lock().give_up_webhook_delivery(1,Some(500),Some("late failure"),at).unwrap();terminal=true;},_=>{w.store.lock().mark_webhook_delivered(1,200,at).unwrap();successful=true;terminal=true;}}
            let row=w.rows()[0].clone();if successful {assert!(row.delivered.is_some());assert_eq!(row.gave_up,None);assert_eq!(row.error,None);assert_eq!(row.status,Some(200));}
            if previous.delivered.is_some() || (previous.gave_up.is_some() && op!=2) {assert_eq!(row,previous);}
            if terminal {assert!(w.store.lock().due_webhook_deliveries_fair(i64::MAX,4,50).unwrap().is_empty());}
        }
    }
    #[test]
    fn private_policy_reload_cannot_reuse_an_old_permitted_connection(flips in prop::collection::vec(any::<bool>(),1..16),hostname in any::<bool>()) {
        runtime().block_on(async {
            let server=Server::new(200).await;let w=World::new(true);let url=if hostname {server.url.replace("127.0.0.1","localhost")}else{server.url.clone()};w.seed(&url,1,1,1,"{}");let delivery=w.store.lock().due_webhook_deliveries_for_test(1000,1).unwrap().remove(0);
            let sequence=[true,false].into_iter().chain(flips);
            for allowed in sequence {w.client.set_allow_private(allowed);let before=server.state.captured.lock().len();let outcome=attempt_delivery(&w.client,&delivery,TIMEOUT).await;assert_eq!(outcome.delivered,allowed,"{:?}",outcome.error);assert_eq!(server.state.captured.lock().len(),before+usize::from(allowed));}
        });
    }
    #[test]
    fn redirects_never_forward_signed_events(status in prop_oneof![Just(301u16),Just(302),Just(303),Just(307),Just(308)]) {
        runtime().block_on(async {
            let destination=Server::new(200).await;let source=Server::new(status).await;*source.state.redirect.lock()=Some(destination.url.clone());let w=World::new(true);w.seed(&source.url,1,1,1,"{}");
            assert_eq!(w.tick(1000,1,TIMEOUT).await.unwrap(),1);assert!(w.rows()[0].gave_up.is_some());assert_eq!(source.state.captured.lock().len(),1);assert!(destination.state.captured.lock().is_empty());
        });
    }
    #[test]
    fn timeout_and_disconnect_are_persisted_and_eventually_recover(kind in 0u8..4,millis in 1u64..16,max in 1u32..5) {
        runtime().block_on(async {
            let server=Server::new(200).await;let mut w=World::new(true);let mut reset_task=None;let url=if kind==3 {let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/hook",listener.local_addr().unwrap());reset_task=Some(tokio::spawn(async move {let (stream,_)=listener.accept().await.unwrap();drop(stream);}));url}else if kind==2 {let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let url=format!("http://{}/hook?secret=do-not-store",listener.local_addr().unwrap());drop(listener);url}else{server.url.clone()};
            w.seed(&url,1,1,1,"{}");if kind<2 {server.state.slow.lock().insert("event_0_0_0".into());}
            let timeout=if kind==0 {Duration::ZERO}else{Duration::from_millis(millis)};assert_eq!(w.tick(1000,max,timeout).await.unwrap(),1);let row=w.rows()[0].clone();assert_eq!(row.attempts,1);assert_eq!(row.delivered,None);assert!(row.error.as_ref().is_some_and(|e|!e.contains("do-not-store")));assert_eq!(row.status,None);
            if let Some(task)=reset_task {task.abort();}server.unblock();w.restart();assert_eq!(w.rows()[0],row);
            if max==1 {assert_eq!(row.gave_up,row.last);assert_eq!(w.tick(10000,max,TIMEOUT).await.unwrap(),0);}else{w.store.lock().conn_for_test().execute("UPDATE webhooks SET url=?1",[&server.url]).unwrap();assert_eq!(w.tick(row.due,max,TIMEOUT).await.unwrap(),1);let row=w.rows()[0].clone();assert_eq!(row.attempts,2);assert_eq!(row.delivered,row.last);assert_eq!(row.error,None);}
        });
    }
    #[test]
    fn cancellation_keeps_completed_writes_and_leaves_slow_rows_retryable(fast in 1usize..7,slow in 1usize..7) {
        runtime().block_on(async {
            let server=Server::new(200).await;let w=World::new(true);w.seed(&server.url,fast+slow,1,1,"{}");
            for t in fast..fast+slow {server.state.slow.lock().insert(format!("event_{t}_0_0"));}
            let db=w.db.clone();let client=w.client.clone();let task=tokio::spawn(async move{run_delivery_tick_on(&db,&client,Duration::from_secs(30),8,1000).await});
            tokio::time::timeout(Duration::from_secs(5),async {loop {if w.rows().iter().filter(|r|r.delivered.is_some()).count()==fast && server.state.captured.lock().len()==fast+slow {break;}tokio::time::sleep(Duration::from_millis(1)).await;}}).await.unwrap();
            task.abort();assert!(task.await.unwrap_err().is_cancelled());let rows=w.rows();assert!(rows[..fast].iter().all(|r|r.attempts==1 && r.delivered.is_some_and(|at|at>=1000)));assert!(rows[fast..].iter().all(|r|r.attempts==0 && r.delivered.is_none()));
            server.unblock();assert_eq!(w.tick(1000,8,TIMEOUT).await.unwrap(),slow);assert!(w.rows().iter().all(|r|r.delivered.is_some()));assert!(server.state.peak.load(Ordering::SeqCst)<=16);
        });
    }
    #[test]
    fn production_worker_drains_multiple_order_events_with_bounded_concurrency(tenants in 1usize..21,orders in 1usize..7,events in 1usize..4) {
        runtime().block_on(async {
            let server=Server::new(200).await;let w=World::new(true);let ids=w.seed(&server.url,tenants,orders,events,"{}");let mut remaining=tenants*orders*events;
            while remaining>0 {let n=w.tick(1000,8,TIMEOUT).await.unwrap();assert!(n>0 && n<=50 && n<=tenants*4 && n<=tenants*orders);remaining-=n;}
            let captures=server.state.captured.lock();assert_eq!(captures.len(),tenants*orders*events);let mut next=HashMap::new();for c in captures.iter(){let v:Value=serde_json::from_slice(&c.body).unwrap();let parts:Vec<usize>=v["event_id"].as_str().unwrap().strip_prefix("event_").unwrap().split('_').map(|s|s.parse().unwrap()).collect();let e=next.entry((parts[0],parts[1])).or_insert(0);assert_eq!(*e,parts[2]);*e+=1;verify(c,std::str::from_utf8(&c.body).unwrap(),v["event_id"].as_str().unwrap());}
            assert!(w.rows().iter().all(|r|r.attempts==1 && r.delivered.is_some_and(|at|at>=1000)));assert!(server.state.peak.load(Ordering::SeqCst)<=16);assert_eq!(ids.len(),tenants);
        });
    }
    #[test]
    fn sql_failures_leave_retryable_rows_and_do_not_discard_other_completed_outcomes(fault in 0usize..32,kind in 0u8..4) {
        runtime().block_on(async {sql_fault_case(fault,kind).await;});
    }
    #[test]
    fn attempt_and_timestamp_limits_do_not_overflow(attempt in prop_oneof![0u32..16,Just(u32::MAX)],now in prop_oneof![1i64..100_000,Just(i64::MAX-1),Just(i64::MAX)]) {
        runtime().block_on(async {
            assert_eq!(backoff_seconds(attempt),60*(1i64<<attempt.min(6)));let server=Server::new(503).await;let w=World::new(true);w.seed(&server.url,1,1,1,"{}");w.store.lock().conn_for_test().execute("UPDATE webhook_deliveries SET attempt_count=?1,next_attempt_at_utc=?2",rusqlite::params![i64::from(attempt),now]).unwrap();
            w.tick(now,u32::MAX,TIMEOUT).await.unwrap();let row=w.rows()[0].clone();assert_eq!(row.attempts,if attempt==u32::MAX {attempt}else{attempt+1});assert_eq!(server.state.captured.lock().len(),usize::from(attempt<u32::MAX));if attempt>=u32::MAX-1 {assert!(row.gave_up.is_some());}else{assert_eq!(row.due,row.last.unwrap().saturating_add(60*(1i64<<attempt.min(6))));}
        });
    }
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn all_http_status_classes_have_correct_terminal_bookkeeping(status in 200u16..600) {
        runtime().block_on(async {
            let server=Server::new(status).await;let w=World::new(true);w.seed(&server.url,1,1,1,"{}");assert_eq!(w.tick(1000,1,TIMEOUT).await.unwrap(),1);
            let row=w.rows()[0].clone();assert_eq!(row.status,Some(status));assert_eq!(row.delivered.is_some(),status<300);assert_eq!(row.gave_up.is_some(),status>=300);assert_eq!(row.error.is_some(),status>=300);
        });
    }
    #[test]
    fn concurrency_limit_is_exercised_with_all_worker_slots_blocked(tenants in 16usize..65) {
        runtime().block_on(async {
            let server=Server::new(200).await;let w=World::new(true);w.seed(&server.url,tenants,1,1,"{}");for t in 0..tenants {server.state.slow.lock().insert(format!("event_{t}_0_0"));}
            let db=w.db.clone();let client=w.client.clone();let task=tokio::spawn(async move {run_delivery_tick_on(&db,&client,Duration::from_secs(30),8,1000).await});
            tokio::time::timeout(Duration::from_secs(5),async {loop {if server.state.captured.lock().len()==16 {break;}tokio::time::sleep(Duration::from_millis(1)).await;}}).await.unwrap();
            assert_eq!(server.state.active.load(Ordering::SeqCst),16);assert!(w.rows().iter().all(|r|r.attempts==0));
            tokio::task::yield_now().await;assert_eq!(server.state.captured.lock().len(),16);server.unblock();assert_eq!(task.await.unwrap().unwrap(),tenants.min(50));assert_eq!(server.state.peak.load(Ordering::SeqCst),16);
            assert_eq!(w.rows().iter().filter(|r|r.delivered.is_some()).count(),tenants.min(50));if tenants>50 {assert_eq!(w.tick(1000,8,TIMEOUT).await.unwrap(),tenants-50);}
        });
    }
    #[test]
    fn legacy_payloads_use_stable_fallback_identity(raw in any::<String>(),valid in any::<bool>(),numeric in any::<u64>()) {
        runtime().block_on(async {
            let server=Server::new(200).await;let w=World::new(true);w.seed(&server.url,1,1,1,"{}");let mut delivery=w.store.lock().due_webhook_deliveries_for_test(1000,1).unwrap().remove(0);
            delivery.payload_json=if valid {serde_json::json!({"event_id":numeric,"message":raw}).to_string()}else{format!("legacy:{raw}")};
            let expected=serde_json::from_str::<Value>(&delivery.payload_json).ok().and_then(|v|v.get("event_id").and_then(Value::as_str).map(str::to_owned)).unwrap_or_else(||delivery.delivery_id.to_string());
            assert!(attempt_delivery(&w.client,&delivery,TIMEOUT).await.delivered);assert!(attempt_delivery(&w.client,&delivery,TIMEOUT).await.delivered);let captures=server.state.captured.lock();assert_eq!(captures.len(),2);for c in captures.iter(){verify(c,&delivery.payload_json,&expected);}
        });
    }
    #[test]
    fn independent_webhooks_and_disable_reload_do_not_break_per_subscription_order(events in 2usize..7,deleted in any::<bool>()) {
        runtime().block_on(async {
            let server=Server::new(503).await;let mut w=World::new(true);w.seed(&server.url,1,1,events,"{}");let first=w.store.lock().due_webhook_deliveries_for_test(1000,1).unwrap().remove(0);
            let tenant=w.store.lock().get_order_tenant_id(&first.order_id).unwrap().unwrap();
            let second=w.store.lock().create_webhook(&tenant,&server.url,"{}","whsec_property",1000).unwrap();
            for e in 0..events {w.store.lock().enqueue_webhook_delivery(&second.id,&first.order_id,"order.paid",&serde_json::json!({"event_id":format!("other_{e}")}).to_string(),1000).unwrap();}
            assert_eq!(w.tick(1000,8,TIMEOUT).await.unwrap(),2);assert_eq!(w.tick(1059,8,TIMEOUT).await.unwrap(),0);let ready=w.rows().iter().map(|r|r.due).max().unwrap();
            w.store.lock().conn_for_test().execute("UPDATE webhooks SET enabled=0 WHERE id=?1",[&first.webhook_id]).unwrap();server.state.status.store(200,Ordering::SeqCst);w.restart();
            for _ in 0..events {assert_eq!(w.tick(ready,8,TIMEOUT).await.unwrap(),1);}
            assert_eq!(w.tick(10000,8,TIMEOUT).await.unwrap(),0);assert!(w.rows()[..events].iter().all(|r|r.delivered.is_none()));
            if deleted {assert!(w.store.lock().delete_webhook(&tenant,&first.webhook_id).unwrap());assert_eq!(w.tick(10000,8,TIMEOUT).await.unwrap(),0);assert_eq!(w.rows().len(),events);}else{
                w.store.lock().conn_for_test().execute("UPDATE webhooks SET enabled=1 WHERE id=?1",[&first.webhook_id]).unwrap();w.restart();for _ in 0..events {assert_eq!(w.tick(ready,8,TIMEOUT).await.unwrap(),1);}assert!(w.rows().iter().all(|r|r.delivered.is_some()));
            }
            let captures=server.state.captured.lock();let mut first_next=0;let mut second_next=0;for c in captures.iter().skip(2) {let id=c.headers["x-monokulo-event-id"].to_str().unwrap();if id.starts_with("other_"){assert_eq!(id,format!("other_{second_next}"));second_next+=1;}else{assert_eq!(id,format!("event_0_0_{first_next}"));first_next+=1;}}
            assert_eq!(second_next,events);assert_eq!(first_next,if deleted {0}else{events});
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn lowered_retry_budgets_retire_without_another_network_attempt(attempts in 1u32..16,max in 0u32..16) {
        runtime().block_on(async {
            let server=Server::new(503).await;let w=World::new(true);w.seed(&server.url,1,1,1,"{}");w.store.lock().conn_for_test().execute("UPDATE webhook_deliveries SET attempt_count=?1,last_response_status=500,last_error='old failure',last_attempted_at_utc=900",[i64::from(attempts)]).unwrap();
            assert_eq!(w.tick(1000,max,TIMEOUT).await.unwrap(),1);let row=w.rows()[0].clone();assert_eq!(server.state.captured.lock().len(),usize::from(attempts<max));assert_eq!(row.attempts,attempts+u32::from(attempts<max));if attempts>=max {assert_eq!(row.gave_up,Some(1000));assert_eq!(row.status,Some(500));assert_eq!(row.error.as_deref(),Some("old failure"));}
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn webhook_deletion_is_tenant_scoped_and_atomic_on_sql_failure(fault in 0usize..24,wrong_tenant in any::<bool>()) {
        deletion_fault_case(fault,wrong_tenant);
    }
}

#[test]
fn a_successful_retry_clears_the_previous_failure_and_rejects_late_failures() {
    let w = World::new(false);
    w.seed("invalid", 1, 1, 1, "{}");
    let s = w.store.lock();
    s.schedule_webhook_retry(1, 1060, Some(500), Some("old error"), 1000)
        .unwrap();
    s.give_up_webhook_delivery(1, Some(500), Some("late exhausted"), 1060)
        .unwrap();
    s.mark_webhook_delivered(1, 200, 1061).unwrap();
    drop(s);
    let successful = w.rows()[0].clone();
    assert_eq!(successful.error, None);
    assert_eq!(successful.gave_up, None);
    assert_eq!(successful.delivered, Some(1061));
    w.store
        .lock()
        .schedule_webhook_retry(1, 2000, Some(500), Some("late failure"), 1062)
        .unwrap();
    w.store
        .lock()
        .give_up_webhook_delivery(1, Some(500), Some("late give up"), 1063)
        .unwrap();
    w.store.lock().mark_webhook_delivered(1, 201, 1064).unwrap();
    assert_eq!(w.rows()[0], successful);
}
#[test]
fn tightening_policy_after_a_localhost_success_blocks_the_next_request() {
    runtime().block_on(async {
        let server = Server::new(200).await;
        let w = World::new(true);
        w.seed(&server.url.replace("127.0.0.1", "localhost"), 1, 1, 1, "{}");
        let delivery = w
            .store
            .lock()
            .due_webhook_deliveries_for_test(1000, 1)
            .unwrap()
            .remove(0);
        assert!(
            attempt_delivery(&w.client, &delivery, TIMEOUT)
                .await
                .delivered
        );
        w.client.set_allow_private(false);
        assert!(
            !attempt_delivery(&w.client, &delivery, TIMEOUT)
                .await
                .delivered
        );
        assert_eq!(server.state.captured.lock().len(), 1);
    });
}
#[test]
fn stored_protocol_header_overrides_are_ignored_before_sending() {
    runtime().block_on(async {
        let server=Server::new(200).await;let w=World::new(true);w.seed(&server.url,1,1,1,r#"{"X-Monokulo-Signature":"bad","X-MONOKULO-EVENT-ID":"bad","x-monokulo-event":"bad","Content-Length":"0","Content-Type":"bad","Host":"bad","Connection":"bad","Transfer-Encoding":"bad","x-merchant":"ok"}"#);let delivery=w.store.lock().due_webhook_deliveries_for_test(1000,1).unwrap().remove(0);assert!(attempt_delivery(&w.client,&delivery,TIMEOUT).await.delivered);verify(&server.state.captured.lock()[0],&delivery.payload_json,"event_0_0_0");
    });
}
#[test]
fn full_width_attempt_count_and_retry_timestamp_do_not_crash_the_worker() {
    runtime().block_on(async {
        let server = Server::new(503).await;
        let w = World::new(true);
        w.seed(&server.url, 1, 1, 1, "{}");
        w.tick(i64::MAX, 8, TIMEOUT).await.unwrap();
        assert_eq!(w.rows()[0].due, i64::MAX);
        w.store
            .lock()
            .conn_for_test()
            .execute(
                "UPDATE webhook_deliveries SET attempt_count=?1",
                [i64::from(u32::MAX)],
            )
            .unwrap();
        w.tick(i64::MAX, u32::MAX, TIMEOUT).await.unwrap();
        assert_eq!(w.rows()[0].attempts, u32::MAX);
        assert!(w.rows()[0].gave_up.is_some());
        assert_eq!(server.state.captured.lock().len(), 1);
    });
}

struct StalledResolver(Arc<AtomicUsize>);
impl reqwest::dns::Resolve for StalledResolver {
    fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::pending())
    }
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn a_stalled_dns_resolution_is_bounded_and_retryable(millis in 1u64..16,max in 1u32..5) {
        runtime().block_on(async {
            let mut w=World::new(true);w.seed("http://stalled.test/hook",1,1,1,"{}");let entered=Arc::new(AtomicUsize::new(0));w.client.private_client=reqwest::Client::builder().no_proxy().dns_resolver(Arc::new(StalledResolver(Arc::clone(&entered)))).build().unwrap();
            let start=std::time::Instant::now();assert_eq!(w.tick(1000,max,Duration::from_millis(millis)).await.unwrap(),1);assert!(start.elapsed()<Duration::from_secs(2));assert_eq!(entered.load(Ordering::SeqCst),1);let row=w.rows()[0].clone();assert_eq!(row.attempts,1);assert_eq!(row.delivered,None);assert_eq!(row.status,None);assert!(row.error.is_some());assert_eq!(row.gave_up.is_some(),max==1);if max>1 {assert_eq!(row.due,row.last.unwrap()+60);}
            let server=Server::new(200).await;w.store.lock().conn_for_test().execute("UPDATE webhooks SET url=?1",[&server.url]).unwrap();w.client=WebhookClient::build().unwrap();w.client.set_allow_private(true);w.restart();assert_eq!(w.tick(row.due,max,TIMEOUT).await.unwrap(),usize::from(max>1));if max>1 {assert!(w.rows()[0].delivered.is_some());}
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn overlapping_ticks_preserve_success_when_a_previous_request_fails_late(status in 400u16..600,max in 1u32..9) {
        runtime().block_on(async {
            let server=Server::new(status).await;let w=World::new(true);w.seed(&server.url,1,1,2,"{}");server.state.slow.lock().insert("event_0_0_0".into());let db=w.db.clone();let client=w.client.clone();let first=tokio::spawn(async move {run_delivery_tick_on(&db,&client,Duration::from_secs(30),max,1000).await});
            tokio::time::timeout(Duration::from_secs(5),async {loop {if server.state.captured.lock().len()==1 {break;}tokio::time::sleep(Duration::from_millis(1)).await;}}).await.unwrap();
            server.state.slow.lock().clear();server.state.status.store(200,Ordering::SeqCst);assert_eq!(w.tick(1000,max,TIMEOUT).await.unwrap(),1);let success=w.rows()[0].clone();assert_eq!(success.attempts,1);assert!(success.delivered.is_some_and(|at|at>=1000));assert_eq!(w.tick(1000,max,TIMEOUT).await.unwrap(),1);
            server.state.release.notify_waiters();assert_eq!(first.await.unwrap().unwrap(),1);assert_eq!(w.rows()[0],success);let captures=server.state.captured.lock();assert_eq!(captures.len(),3);assert_eq!(captures[0].body,captures[1].body);assert_ne!(captures[1].body,captures[2].body);
        });
    }
}

fn deletion_fault_case(fault: usize, wrong_tenant: bool) -> bool {
    let w = World::new(false);
    w.seed("invalid", 2, 1, 2, "{}");
    let due = w
        .store
        .lock()
        .due_webhook_deliveries_for_test(1000, 4)
        .unwrap();
    let target = &due[0];
    let tenant = w
        .store
        .lock()
        .get_order_tenant_id(&due[if wrong_tenant { 2 } else { 0 }].order_id)
        .unwrap()
        .unwrap();
    let before = w.rows();
    let trace = w.store.lock().fail_nth_access(Some(fault));
    let result = w.store.lock().delete_webhook(&tenant, &target.webhook_id);
    w.store.lock().fail_nth_access(None);
    let denied = trace.denied.load(Ordering::Relaxed) > 0;
    if denied {
        assert!(result.is_err());
        assert_eq!(w.rows(), before);
    } else {
        assert_eq!(result.unwrap(), !wrong_tenant);
        if wrong_tenant {
            assert_eq!(w.rows(), before);
        } else {
            assert_eq!(w.rows(), before[2..]);
        }
    }
    assert!(w.store.lock().list_webhooks(&tenant).unwrap().len() <= 1);
    denied
}
#[test]
fn deletion_recovers_at_every_reached_sql_boundary() {
    for wrong_tenant in [false, true] {
        let mut reached = 0;
        for fault in 0..64 {
            if !deletion_fault_case(fault, wrong_tenant) {
                break;
            }
            reached += 1;
        }
        assert!(reached >= 4);
    }
}

async fn sql_fault_case(fault: usize, kind: u8) -> bool {
    let server = Server::new(if kind == 0 { 200 } else { 503 }).await;
    let mut w = World::new(false);
    w.inline();
    w.seed(&server.url, 3, 1, 1, "{}");
    if kind == 3 {
        w.store
            .lock()
            .conn_for_test()
            .execute("UPDATE webhook_deliveries SET attempt_count=1", [])
            .unwrap();
    }
    let trace = w.store.lock().fail_nth_access(Some(fault));
    let result = w.tick(1000, if kind >= 2 { 1 } else { 8 }, TIMEOUT).await;
    w.store.lock().fail_nth_access(None);
    let reached = trace.denied.load(Ordering::Relaxed) > 0;
    assert_eq!(result.is_err(), reached);
    let rows = w.rows();
    assert!(rows.iter().all(|r| r.attempts <= 1));
    if reached
        && kind == 3
        && trace
            .action
            .lock()
            .as_ref()
            .is_some_and(|a| a.starts_with("Update"))
    {
        assert_eq!(rows.iter().filter(|r| r.gave_up.is_some()).count(), 2);
    }
    if reached && server.state.captured.lock().len() == 3 {
        assert_eq!(
            rows.iter().filter(|r| r.attempts == 1).count(),
            2,
            "later outcomes must still commit"
        );
    }
    server.state.status.store(200, Ordering::SeqCst);
    w.restart();
    w.tick(10000, 8, TIMEOUT).await.unwrap();
    assert!(w
        .rows()
        .iter()
        .all(|r| r.delivered.is_some() || r.gave_up.is_some()));
    reached
}
#[test]
fn every_reached_sql_boundary_recovers_for_all_outcome_types() {
    runtime().block_on(async {
        for kind in 0..4 {
            let mut reached = 0;
            for fault in 0..64 {
                if !sql_fault_case(fault, kind).await {
                    break;
                }
                reached += 1;
            }
            assert!(reached >= 4);
        }
    });
}

#[test]
fn delivery_crash_child() {
    let Ok(path) = std::env::var("MONOKULO_WEBHOOK_CRASH_PATH") else {
        return;
    };
    runtime().block_on(async {
        let s = Store::open_file(&path).unwrap();
        let db = Db::open(&path, &s).unwrap();
        let client = WebhookClient::build().unwrap();
        client.set_allow_private(true);
        let max = std::env::var("MONOKULO_WEBHOOK_CRASH_MAX")
            .unwrap()
            .parse()
            .unwrap();
        run_delivery_tick_on(&db, &client, TIMEOUT, max, 1000)
            .await
            .unwrap();
        panic!("crash point not reached");
    });
}
async fn crash_case(kind: u8, after: bool) {
    let server = Server::new(if kind == 0 { 200 } else { 503 }).await;
    let mut w = World::new(true);
    w.seed(&server.url, 1, 1, 1, "{}");
    let suffix = if after {
        "after_commit"
    } else {
        "before_commit"
    };
    let operation = ["delivered", "retry", "give_up", "exhausted"][kind as usize];
    if kind == 3 {
        w.store
            .lock()
            .conn_for_test()
            .execute("UPDATE webhook_deliveries SET attempt_count=1", [])
            .unwrap();
    }
    let point = format!("webhooks.{operation}.{suffix}");
    let mut child = CrashChild(Some(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "webhook_delivery::properties::delivery_crash_child",
                "--nocapture",
            ])
            .env("MONOKULO_WEBHOOK_CRASH_PATH", &w.path.0)
            .env(
                "MONOKULO_WEBHOOK_CRASH_MAX",
                if kind >= 2 { "1" } else { "8" },
            )
            .env("MONOKULO_PROPERTY_CRASH_PATH", &w.path.0)
            .env("MONOKULO_PROPERTY_CRASH_POINT", &point)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    child.rendezvous(&w.path.0, &point).await;
    assert!(!child.finish().status.success());
    w.restart();
    let row = w.rows()[0].clone();
    assert_eq!(row.attempts, if kind == 3 { 1 } else { u32::from(after) });
    assert_eq!(row.delivered.is_some(), after && kind == 0);
    assert_eq!(row.gave_up.is_some(), after && kind >= 2);
    if after && kind == 1 {
        assert_eq!(row.due, row.last.unwrap() + 60);
    }
    assert_eq!(
        w.store
            .lock()
            .conn_for_test()
            .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    let before = server.state.captured.lock().len();
    assert_eq!(before, usize::from(kind != 3));
    server.state.status.store(200, Ordering::SeqCst);
    let retry = !(after && kind != 1);
    assert_eq!(w.tick(10000, 8, TIMEOUT).await.unwrap(), usize::from(retry));
    let captures = server.state.captured.lock();
    assert_eq!(captures.len(), before + usize::from(retry));
    for c in captures.iter() {
        verify(c, std::str::from_utf8(&c.body).unwrap(), "event_0_0_0");
        assert_eq!(c.body, captures[0].body);
    }
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn process_death_preserves_at_least_once_delivery_and_stable_event_identity(kind in 0u8..4,after in any::<bool>()) {runtime().block_on(crash_case(kind,after));}
}
#[test]
fn all_eight_delivery_durability_boundaries_are_exercised() {
    runtime().block_on(async {
        for kind in 0..4 {
            for after in [false, true] {
                crash_case(kind, after).await;
            }
        }
    });
}

#[test]
fn proxy_policy_child() {
    if std::env::var_os("MONOKULO_WEBHOOK_PROXY_CHILD").is_none() {
        return;
    }
    runtime().block_on(async {
        let w = World::new(true);
        w.client.set_allow_private(false);
        w.seed("http://localhost:9/hook", 1, 1, 1, "{}");
        let delivery = w
            .store
            .lock()
            .due_webhook_deliveries_for_test(1000, 1)
            .unwrap()
            .remove(0);
        let outcome = attempt_delivery(&w.client, &delivery, TIMEOUT).await;
        assert!(
            !outcome.delivered,
            "a proxy bypassed the private-destination check"
        );
        assert!(outcome.error.is_some());
    });
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn system_proxy_settings_cannot_bypass_destination_checks(proxy_key in 0usize..4) {
        runtime().block_on(proxy_case(proxy_key));
    }
}

async fn proxy_case(proxy_key: usize) {
    let proxy = Server::new(200).await;
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args([
        "--exact",
        "webhook_delivery::properties::proxy_policy_child",
        "--nocapture",
    ]);
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        command.env_remove(key);
    }
    command
        .env(
            ["HTTP_PROXY", "ALL_PROXY", "http_proxy", "all_proxy"][proxy_key],
            &proxy.url,
        )
        .env("NO_PROXY", "")
        .env("MONOKULO_WEBHOOK_PROXY_CHILD", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = CrashChild(Some(command.spawn().unwrap()));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
    let output = child.finish();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(proxy.state.captured.lock().is_empty());
}
#[test]
fn every_proxy_environment_variant_preserves_destination_checks() {
    runtime().block_on(async {
        for proxy_key in 0..4 {
            proxy_case(proxy_key).await;
        }
    });
}

#[path = "webhook_scale_properties.rs"]
mod scale;

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/webhook_properties.txt"
        ),
    )
}
