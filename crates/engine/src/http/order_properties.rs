//! Real authenticated HTTP requests, actual address derivation and SQLite.
use super::*;
use crate::property_support::{config, custody_arc, runtime, GateCustody, TempFile};
use crate::store::{Database, Db, ReadStorePool, SharedStore, TenantId};
use proptest::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;

type OrderRow = (String, u32, String, u64, Option<String>, Option<u64>);
type Snapshot = Vec<(u32, Vec<OrderRow>)>;

struct World {
    state: AppState,
    router: Router,
    tenants: Vec<TestTenant>,
    ids: Vec<TenantId>,
    store: SharedStore,
    custody: Arc<GateCustody>,
    path: TempFile,
}
impl World {
    async fn new(count: usize, worker: bool) -> Self {
        let path = TempFile::new();
        let store = Store::open_file(&path.0).unwrap().into_shared();
        let custody = Arc::new(GateCustody::default());
        let mut state = AppState::for_tests_with_store(Arc::clone(&store));
        if worker {
            let s = store.lock();
            state.db = Database::from_parts(
                Db::open(&path.0, &s).unwrap(),
                ReadStorePool::open(&path.0, 2).unwrap(),
                &s,
            );
        }
        state.custody.backends = custody_arc(&custody);
        let router = build_router(state.clone(), 1_000_000);
        let mut tenants = vec![];
        let mut ids = vec![];
        for i in 0..count {
            let t = create_tenant(&router, (i * 2 + 1) as u8).await;
            ids.push(
                store
                    .lock()
                    .find_tenant_by_secret_token(&shared::auth::RawToken::presented(
                        &t.secret_token,
                    ))
                    .unwrap()
                    .unwrap()
                    .id,
            );
            tenants.push(t);
        }
        Self {
            state,
            router,
            tenants,
            ids,
            store,
            custody,
            path,
        }
    }
    fn restart(&mut self) {
        self.custody = Arc::new(GateCustody::default());
        self.store = Store::open_file(&self.path.0).unwrap().into_shared();
        self.state = AppState::for_tests_with_store(Arc::clone(&self.store));
        self.state.custody.backends = custody_arc(&self.custody);
        self.router = build_router(self.state.clone(), 1_000_000);
    }
    fn snapshot(&self) -> Snapshot {
        let s = self.store.lock();
        self.ids
            .iter()
            .map(|id| {
                let t = s.get_tenant_by_id(id).unwrap().unwrap();
                let mut rows: Vec<_> = s
                    .list_orders(id, None, 1000, None)
                    .unwrap()
                    .into_iter()
                    .map(|o| {
                        (
                            o.id.to_string(),
                            o.minor_index,
                            o.address,
                            o.xmr_amount_piconero,
                            o.merchant_order_id,
                            o.confirmations_required_override,
                        )
                    })
                    .collect();
                rows.sort();
                (t.next_minor_index, rows)
            })
            .collect()
    }
    fn invariants(&self) {
        let mut addresses = HashSet::new();
        for (tenant, (next, rows)) in self.snapshot().into_iter().enumerate() {
            assert_eq!(
                next as usize,
                rows.len() + 1,
                "allocation advanced without a durable order"
            );
            let mut indices: Vec<_> = rows.iter().map(|r| r.1).collect();
            indices.sort_unstable();
            assert_eq!(indices, (1..next).collect::<Vec<_>>());
            for row in rows {
                let seed = (tenant * 2 + 1) as u8;
                let pair = monero::ViewPair {
                    view: PrivateKey::from_slice(&valid_scalar_bytes(seed)).unwrap(),
                    spend: PublicKey::from_private_key(
                        &PrivateKey::from_slice(&valid_scalar_bytes(seed + 1)).unwrap(),
                    ),
                };
                let expected = monero::cryptonote::subaddress::get_subaddress(
                    &pair,
                    crate::key_custody::SubaddressIndex {
                        major: 0,
                        minor: row.1,
                    },
                    Some(Network::Mainnet),
                );
                assert_eq!(
                    row.2,
                    expected.to_string(),
                    "order points to the wrong wallet or index"
                );
                assert!(addresses.insert(row.2), "two purchases share an address");
            }
        }
    }
}
fn body(
    key: Option<String>,
    amount: u64,
    reference: u8,
    confirmations: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({"idempotency_key":key.map(serde_json::Value::String),"xmr_amount_piconero":amount,"merchant_order_id":format!("purchase-{reference}"),"confirmations_required":confirmations,"description":"first description"})
}
async fn request(
    router: Router,
    token: String,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let response = router
        .oneshot(json_request(
            "POST",
            "/api/v1/admin/tenant/orders",
            Some(&token),
            &body,
        ))
        .await
        .unwrap();
    (response.status(), body_json(response).await)
}
async fn create(
    w: &World,
    tenant: usize,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    request(
        w.router.clone(),
        w.tenants[tenant].secret_token.clone(),
        payload,
    )
    .await
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn order_histories_preserve_tenant_scoped_idempotency_and_addresses(
        count in 2usize..5, base in 1u64..1_000_000_000_000,
        events in proptest::collection::vec((0usize..4,0u8..8,0u8..8),1..25),
    ) {
        runtime().block_on(async {
            let mut w = World::new(count,false).await;
            let mut model = BTreeMap::<(usize,u8),serde_json::Value>::new();
            for (tenant,key,kind) in events {
                let tenant = tenant % count;
                let payload = body(Some(format!("key-{key}")),base+u64::from(key),key,Some(u64::from(key)));
                if kind == 4 { w.restart(); }
                if kind == 5 {
                    let handle = w.state.custody.wallet_handles.read().get(&w.ids[tenant]).copied();
                    if let Some(handle) = handle { w.custody.inner.remove_wallet(handle).await.unwrap(); }
                }
                if kind == 6 {
                    w.custody.mode.store(1,Ordering::Relaxed);
                    let before=w.snapshot(); let (status,_)=create(&w,tenant,payload.clone()).await;
                    assert!(status.is_server_error()); assert_eq!(w.snapshot(),before);
                    w.custody.mode.store(0,Ordering::Relaxed);
                }
                let before=w.snapshot();
                if kind == 2 {
                    let mut invalid=payload.clone(); invalid["xmr_amount_piconero"]=0.into();
                    assert_eq!(create(&w,tenant,invalid).await.0,StatusCode::BAD_REQUEST);
                    assert_eq!(w.snapshot(),before);
                }
                if kind == 3 && model.contains_key(&(tenant,key)) {
                    let mut conflict=payload.clone(); conflict["merchant_order_id"]="different purchase".into();
                    assert_eq!(create(&w,tenant,conflict).await.0,StatusCode::CONFLICT);
                    assert_eq!(w.snapshot(),before);
                }
                let (status,first)=create(&w,tenant,payload.clone()).await; assert_eq!(status,StatusCode::OK,"{first}");
                assert_eq!(first["xmr_amount_piconero"].as_u64(),Some(base+u64::from(key)));
                {
                    let id=crate::store::OrderId::from(first["order_id"].as_str().unwrap());
                    let s=w.store.lock();let o=s.get_order(&w.ids[tenant],&id).unwrap().unwrap();
                    assert_eq!(o.xmr_amount_piconero,base+u64::from(key));
                    assert_eq!(o.merchant_order_id,Some(format!("purchase-{key}")));
                    assert_eq!(o.confirmations_required_override,Some(u64::from(key)));
                    assert_eq!(first["address"].as_str(),Some(o.address.as_str()));
                    assert_eq!(first["expires_at"].as_i64(),Some(o.expires_at));
                }
                if let Some(expected)=model.get(&(tenant,key)) { assert_eq!(&first,expected); } else { model.insert((tenant,key),first.clone()); }
                // A lost HTTP response is retried just like any other request.
                let mut retry=payload; retry["description"]="changed non-identity metadata".into();
                let (status,again)=create(&w,tenant,retry).await; assert_eq!(status,StatusCode::OK); assert_eq!(first,again);
                w.invariants();
                assert_eq!(w.snapshot().iter().map(|(_,r)|r.len()).sum::<usize>(),model.len());
            }
        });
    }
    #[test]
    fn request_validation_is_atomic_at_integer_and_key_boundaries(
        amount in prop_oneof![Just(0),Just(super::super::orders::MAX_ORDER_PICONERO),Just(super::super::orders::MAX_ORDER_PICONERO+1),any::<u64>()],
        key in prop_oneof!["[!-~]{1,128}",Just(String::new()),Just("x".repeat(129)),Just("contains space".to_owned()),Just("\u{e9}".to_owned()),Just("a\nb".to_owned())],
        confirmations in prop_oneof![Just(None),any::<u64>().prop_map(Some),Just(Some(0)),Just(Some(super::super::admin::MAX_CONFIRMATIONS_REQUIRED))],
    ) {
        runtime().block_on(async {
            let w=World::new(1,false).await; let before=w.snapshot();
            let valid=amount>0 && amount<=super::super::orders::MAX_ORDER_PICONERO && !key.is_empty() && key.len()<=128 && key.bytes().all(|b|b.is_ascii_graphic()) && confirmations.is_none_or(|c|c<=super::super::admin::MAX_CONFIRMATIONS_REQUIRED);
            let (status,_)=create(&w,0,body(Some(key),amount,0,confirmations)).await;
            assert_eq!(status,if valid {StatusCode::OK} else {StatusCode::BAD_REQUEST});
            if !valid {assert_eq!(w.snapshot(),before);} w.invariants();
        });
    }
    #[test]
    fn concurrent_creation_and_retries_use_the_production_database_worker(
        count in 2usize..33, tenants in 1usize..4, distinct in any::<bool>(), amount in 1u64..10000,
    ) {
        runtime().block_on(async {
            let w=World::new(tenants,true).await;
            let jobs=(0..count).map(|i| {
                let router=w.router.clone();let token=w.tenants[i%tenants].secret_token.clone();
                let key=if distinct {i} else {i%tenants};
                let payload=body(Some(format!("race-{key}")),amount,0,None);
                async move {
                    for _ in 0..count+2 {
                        let (status,reply)=request(router.clone(),token.clone(),payload.clone()).await;
                        if status==StatusCode::OK {return reply;}
                        assert!(status.is_server_error());tokio::task::yield_now().await;
                    }
                    panic!("valid order never recovered from contention");
                }
            });
            let replies=futures_util::future::join_all(jobs).await;
            let expected=if distinct {count} else {tenants.min(count)};
            assert_eq!(replies.iter().map(|r|r["order_id"].as_str().unwrap()).collect::<HashSet<_>>().len(),expected);
            w.invariants(); assert_eq!(w.snapshot().iter().map(|(_,r)|r.len()).sum::<usize>(),expected);
        });
    }
    #[test]
    fn sql_failures_and_lost_results_never_burn_addresses(
        amount in 1u64..10000, at in 0usize..100, restart in any::<bool>(),
    ) {
        runtime().block_on(async {
            let mut w=World::new(1,false).await;
            let payload=body(Some("faulted".to_owned()),amount,0,None);
            let trace=w.store.lock().fail_nth_access(Some(at));
            let (status,_)=create(&w,0,payload.clone()).await;
            w.store.lock().fail_nth_access(None); trace.assert_outcome(at);
            assert!(status==StatusCode::OK || status.is_server_error()); w.invariants();
            if restart {w.restart();}
            let (status,first)=create(&w,0,payload.clone()).await; assert_eq!(status,StatusCode::OK);
            assert_eq!(create(&w,0,payload).await.1,first); w.invariants(); assert_eq!(w.snapshot()[0].1.len(),1);
        });
    }
    #[test]
    fn cancelling_during_derivation_preserves_the_address_for_retry(amount in 1u64..10000,restart in any::<bool>()) {
        runtime().block_on(async {
            let mut w=World::new(1,false).await; let before=w.snapshot();
            w.custody.mode.store(2,Ordering::Relaxed);
            let payload=body(Some("cancelled".to_owned()),amount,0,None);
            let task=tokio::spawn(request(w.router.clone(),w.tenants[0].secret_token.clone(),payload.clone()));
            tokio::time::timeout(std::time::Duration::from_secs(5),w.custody.entered.notified()).await.unwrap(); task.abort(); assert!(task.await.unwrap_err().is_cancelled());
            assert_eq!(w.snapshot(),before); assert!(w.custody.attempted.load(Ordering::Relaxed)>0);
            w.custody.mode.store(0,Ordering::Relaxed); if restart {w.restart();}
            assert_eq!(create(&w,0,payload).await.0,StatusCode::OK); w.invariants();
        });
    }
    #[test]
    fn exhaustion_and_mismatched_claims_cannot_corrupt_allocation(offset in 0u32..4,mismatch in any::<bool>()) {
        runtime().block_on(async {
            let w=World::new(1,false).await; let next=u32::MAX-offset;
            w.store.lock().conn_for_test().execute("UPDATE tenants SET next_minor_index=?1",[i64::from(next)]).unwrap();
            let new=crate::store::NewOrder { tenant_id:w.ids[0].clone(),minor_index:if mismatch {next.saturating_sub(1)} else {next},address:"derived".to_owned(),xmr_amount_piconero:1,idempotency_key:Some("boundary".to_owned()),merchant_order_id:None,description:None,confirmations_required_override:None,created_at:1,expires_at:2 };
            let result=w.store.lock().create_order_claiming_minor_index(next,&new);
            if mismatch || next==u32::MAX {
                assert!(result.is_err()); assert_eq!(w.store.lock().peek_next_minor_index(&w.ids[0]).unwrap(),next);
                assert!(w.store.lock().list_orders(&w.ids[0],None,10,None).unwrap().is_empty());
            } else { assert!(result.unwrap().is_some());assert_eq!(w.store.lock().get_tenant_by_id(&w.ids[0]).unwrap().unwrap().next_minor_index,next+1); }
        });
    }
}

#[test]
fn every_sql_denial_during_order_creation_is_atomic_and_retryable() {
    runtime().block_on(async {
        for at in 0..100 {
            let w = World::new(1, false).await;
            let payload = body(Some("sweep".to_owned()), 7, 0, None);
            let trace = w.store.lock().fail_nth_access(Some(at));
            let _ = create(&w, 0, payload.clone()).await;
            w.store.lock().fail_nth_access(None);
            trace.assert_outcome(at);
            w.invariants();
            let (status, reply) = create(&w, 0, payload).await;
            assert_eq!(status, StatusCode::OK, "denial at {at}: {reply}");
            w.invariants();
            assert_eq!(w.snapshot()[0].1.len(), 1);
            if trace.denied.load(Ordering::Relaxed) == 0 {
                break;
            }
        }
    });
}

#[test]
fn order_creation_crash_child() {
    let Ok(path) = std::env::var("MONOKULO_ORDER_CRASH_PATH") else {
        return;
    };
    runtime().block_on(async {
        let store = Store::open_file(&path).unwrap().into_shared();
        let router = build_router(AppState::for_tests_with_store(store), 1_000_000);
        let token = std::env::var("MONOKULO_ORDER_CRASH_TOKEN").unwrap();
        let amount = std::env::var("MONOKULO_ORDER_CRASH_AMOUNT")
            .unwrap()
            .parse()
            .unwrap();
        let _ = request(
            router,
            token,
            body(Some("crash".to_owned()), amount, 0, None),
        )
        .await;
        panic!("order crash boundary was not reached");
    });
}
proptest! {
    #![proptest_config(config())]
    #[test]
    fn process_death_at_order_commit_preserves_atomic_allocation(amount in 1u64..10000,after in any::<bool>()) {
        runtime().block_on(async {
            let mut w=World::new(1,false).await;
            let point=if after {"orders.after_commit"} else {"orders.before_commit"};
            let mut child=crate::property_support::CrashChild(Some(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","http::tests::properties::order_creation_crash_child","--nocapture"])
                .env("MONOKULO_ORDER_CRASH_PATH",&w.path.0).env("MONOKULO_ORDER_CRASH_TOKEN",&w.tenants[0].secret_token)
                .env("MONOKULO_ORDER_CRASH_AMOUNT",amount.to_string()).env("MONOKULO_PROPERTY_CRASH_PATH",&w.path.0)
                .env("MONOKULO_PROPERTY_CRASH_POINT",point).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).spawn().unwrap()));
            child.rendezvous(&w.path.0,point).await;assert!(!child.finish().status.success());w.restart();
            assert_eq!(w.snapshot()[0].1.len(),usize::from(after));w.invariants();
            assert_eq!(w.store.lock().conn_for_test().query_row("PRAGMA integrity_check",[],|r|r.get::<_,String>(0)).unwrap(),"ok");
            let payload=body(Some("crash".to_owned()),amount,0,None);
            let (status,first)=create(&w,0,payload.clone()).await;assert_eq!(status,StatusCode::OK);
            assert_eq!(create(&w,0,payload).await.1,first);w.invariants();assert_eq!(w.snapshot()[0].1.len(),1);
        });
    }
    #[test]
    fn disabling_a_tenant_during_derivation_cannot_publish_an_order(amount in 1u64..10000) {
        runtime().block_on(async {
            let w=World::new(1,false).await;w.custody.mode.store(2,Ordering::Relaxed);
            let task=tokio::spawn(request(w.router.clone(),w.tenants[0].secret_token.clone(),body(Some("disabled".to_owned()),amount,0,None)));
            tokio::time::timeout(std::time::Duration::from_secs(5),w.custody.entered.notified()).await.unwrap();w.store.lock().disable_tenant(&w.ids[0],1).unwrap();
            w.custody.mode.store(0,Ordering::Relaxed);w.custody.release.notify_one();
            assert!(!task.await.unwrap().0.is_success());
            assert!(w.store.lock().list_orders(&w.ids[0],None,10,None).unwrap().is_empty());
            assert_eq!(w.store.lock().get_tenant_by_id(&w.ids[0]).unwrap().unwrap().next_minor_index,1);
        });
    }
}

#[test]
fn exhausted_counter_still_allows_idempotent_replays_and_never_wraps() {
    runtime().block_on(async {
        let w = World::new(1, false).await;
        let payload = body(Some("last-order".to_owned()), 1, 0, None);
        let first = create(&w, 0, payload.clone()).await.1;
        w.store
            .lock()
            .conn_for_test()
            .execute(
                "UPDATE tenants SET next_minor_index=?1",
                [i64::from(u32::MAX)],
            )
            .unwrap();
        assert_eq!(create(&w, 0, payload).await, (StatusCode::OK, first));
        assert_eq!(
            create(&w, 0, body(Some("new".to_owned()), 1, 0, None))
                .await
                .0,
            StatusCode::CONFLICT
        );
        w.store.lock().allocate_minor_index(&w.ids[0]).unwrap_err();
        w.store
            .lock()
            .conn_for_test()
            .execute(
                "UPDATE tenants SET next_minor_index=?1",
                [i64::from(u32::MAX) + 1],
            )
            .unwrap();
        w.store.lock().peek_next_minor_index(&w.ids[0]).unwrap_err();
    });
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn requests_without_keys_create_distinct_purchases(count in 2usize..17,amount in 1u64..10000) {
        runtime().block_on(async {
            let w=World::new(1,false).await;let mut ids=HashSet::new();
            for _ in 0..count {
                let (status,reply)=create(&w,0,body(None,amount,0,None)).await;
                assert_eq!(status,StatusCode::OK);assert!(ids.insert(reply["order_id"].as_str().unwrap().to_owned()));w.invariants();
            }
            assert_eq!(w.snapshot()[0].1.len(),count);
        });
    }
    #[test]
    fn every_purchase_identity_field_rejects_conflicting_replays(amount in 1u64..10000,field in 0u8..3,confirmations in 0u64..20) {
        runtime().block_on(async {
            let w=World::new(1,false).await;let mut payload=body(Some("identity".to_owned()),amount,0,Some(confirmations));
            assert_eq!(create(&w,0,payload.clone()).await.0,StatusCode::OK);let before=w.snapshot();
            match field {0=>payload["xmr_amount_piconero"]=(amount+1).into(),1=>payload["merchant_order_id"]="other".into(),_=>payload["confirmations_required"]=(confirmations+1).into()}
            assert_eq!(create(&w,0,payload).await.0,StatusCode::CONFLICT);assert_eq!(w.snapshot(),before);w.invariants();
        });
    }
}

#[path = "authorization_properties.rs"]
mod authorization;
