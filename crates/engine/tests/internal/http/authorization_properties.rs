//! Permission histories through the production router, without token injection.
use super::*;
use crate::store::OrderId;

struct Principal {
    public: String,
    secret: String,
    old: Vec<String>,
    enabled: bool,
    id: TenantId,
    order: String,
    webhook: String,
}
struct World {
    router: Router,
    store: SharedStore,
    principals: Vec<Principal>,
    _path: TempFile,
}
impl World {
    async fn new(count: usize, worker: bool) -> Self {
        Self::with_network(count, worker, None).await
    }
    async fn with_network(count: usize, worker: bool, node_state: Option<AppState>) -> Self {
        let fixture = node_state.is_some();
        let path = TempFile::new();
        let store = Store::open_file(&path.0).unwrap().into_shared();
        let mut state = AppState::for_tests_with_store(Arc::clone(&store));
        if let Some(node_state) = node_state {
            state.networks = node_state.networks;
        }
        if worker {
            let worker_db = Db::open(&path.0, &store.lock()).unwrap();
            let readers = ReadStorePool::open(&path.0, 2).unwrap();
            state.db = Database::from_parts(worker_db, readers, &store.lock());
        }
        let setup = build_router(state.clone(), 1 << 20);
        let router = crate::http::build_router(state, 1 << 20);
        let mut principals = Vec::new();
        for i in 0..count {
            let tenant = if fixture && i == 0 {
                create_fixture_tenant(&setup).await
            } else {
                create_tenant(&setup, (i as u8) * 2 + 1).await
            };
            let id = store
                .lock()
                .find_tenant_by_secret_token(&shared::auth::RawToken::presented(
                    &tenant.secret_token,
                ))
                .unwrap()
                .unwrap()
                .id;
            let order=setup.clone().oneshot(json_request("POST","/api/v1/admin/tenant/orders",Some(&tenant.secret_token),&serde_json::json!({"xmr_amount_piconero":100+i,"merchant_order_id":format!("private-purchase-{i}")}))).await.unwrap();
            assert_eq!(order.status(), StatusCode::OK);
            let order = body_json(order).await["order_id"]
                .as_str()
                .unwrap()
                .to_owned();
            let hook = setup
                .clone()
                .oneshot(json_request(
                    "POST",
                    "/api/v1/admin/tenant/webhooks",
                    Some(&tenant.secret_token),
                    &serde_json::json!({"url":format!("https://merchant-{i}.example/hook")}),
                ))
                .await
                .unwrap();
            assert_eq!(hook.status(), StatusCode::OK);
            let webhook = body_json(hook).await["webhook_id"]
                .as_str()
                .unwrap()
                .to_owned();
            principals.push(Principal {
                public: tenant.public_key,
                secret: tenant.secret_token,
                old: vec![],
                enabled: true,
                id,
                order,
                webhook,
            });
        }
        Self {
            router,
            store,
            principals,
            _path: path,
        }
    }
    fn snapshot(&self) -> String {
        self.store.lock().dump_for_test()
    }
    async fn send(
        &self,
        method: &str,
        uri: &str,
        engine: Option<&str>,
        auth: Option<&str>,
        body: &serde_json::Value,
    ) -> axum::response::Response {
        let mut request = json_request(method, uri, None, body);
        if let Some(value) = engine {
            request
                .headers_mut()
                .insert(shared::auth::ENGINE_TOKEN_HEADER, value.parse().unwrap());
        }
        if let Some(value) = auth {
            request
                .headers_mut()
                .insert("authorization", value.parse().unwrap());
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.router.clone().oneshot(request),
        )
        .await
        .unwrap()
        .unwrap()
    }
    async fn rotate(&mut self, i: usize) {
        let secret = self.principals[i].secret.clone();
        let response = self
            .send(
                "POST",
                "/api/v1/admin/tenant/rotate-secret",
                Some(TEST_ENGINE_TOKEN),
                Some(&format!("Bearer {secret}")),
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let new = body_json(response).await["secret_token"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_ne!(new, secret);
        self.principals[i].old.push(secret);
        self.principals[i].secret = new;
    }
    async fn disable(&mut self, i: usize) {
        let response = self
            .send(
                "DELETE",
                "/api/v1/admin/tenant",
                Some(TEST_ENGINE_TOKEN),
                Some(&format!("Bearer {}", self.principals[i].secret)),
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        self.principals[i].enabled = false;
    }
    fn invalid_auth(&self, kind: u8, noise: &str) -> Option<String> {
        let a = &self.principals[0];
        let last = self.principals.last()?;
        match kind % 8 {
            0 => None,
            1 => Some(format!("Bearer {}", a.public)),
            2 => Some(format!("Bearer {}", a.old.last()?)),
            3 => Some(format!("Bearer {}", last.secret)),
            4 => Some(format!("Basic {}", a.secret)),
            5 => Some(format!("Bearer invalid-{noise}")),
            6 => Some(format!("Bearer {}x", a.secret)),
            _ => Some(format!("Bearer {TEST_ENGINE_TOKEN}")),
        }
    }
}

struct Route {
    method: &'static str,
    uri: String,
    tenant: bool,
}
fn routes(p: &Principal) -> Vec<Route> {
    let tenant = [
        ("GET", "/api/v1/admin/tenant".into()),
        ("PATCH", "/api/v1/admin/tenant".into()),
        ("DELETE", "/api/v1/admin/tenant".into()),
        ("POST", "/api/v1/admin/tenant/rotate-secret".into()),
        ("PUT", "/api/v1/admin/tenant/key-custody".into()),
        ("GET", "/api/v1/admin/tenant/orders".into()),
        ("POST", "/api/v1/admin/tenant/orders".into()),
        ("GET", format!("/api/v1/admin/tenant/orders/{}", p.order)),
        (
            "POST",
            format!("/api/v1/admin/tenant/orders/{}/refund-address", p.order),
        ),
        ("POST", "/api/v1/admin/tenant/payments/lookup".into()),
        ("GET", "/api/v1/admin/tenant/webhooks".into()),
        ("POST", "/api/v1/admin/tenant/webhooks".into()),
        (
            "DELETE",
            format!("/api/v1/admin/tenant/webhooks/{}", p.webhook),
        ),
        ("GET", "/api/v1/admin/tenant/events".into()),
    ];
    let global = [
        ("POST", "/api/v1/admin/tenants"),
        ("GET", "/status"),
        ("GET", "/api/v1/admin/settings"),
        ("POST", "/api/v1/admin/settings"),
        ("POST", "/api/v1/admin/settings/reload"),
        ("DELETE", "/api/v1/admin/proof/mainnet/anchor"),
        ("GET", "/api/v1/admin/logs"),
        ("GET", "/api/v1/admin/logs/trace/unknown"),
        ("GET", "/api/v1/admin/logs/histogram"),
        ("GET", "/api/v1/admin/logs/attributes"),
        ("GET", "/api/v1/admin/engine/activity"),
    ];
    tenant
        .into_iter()
        .map(|(method, uri)| Route {
            method,
            uri,
            tenant: true,
        })
        .chain(global.into_iter().map(|(method, uri)| Route {
            method,
            uri: uri.into(),
            tenant: false,
        }))
        .collect()
}
async fn denied(
    w: &World,
    route: &Route,
    engine: Option<&str>,
    auth: Option<&str>,
    body: &serde_json::Value,
) {
    let before = w.snapshot();
    let response = w.send(route.method, &route.uri, engine, auth, body).await;
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "{} {} escaped authentication",
        route.method,
        route.uri
    );
    let text = body_json(response).await.to_string();
    for p in &w.principals {
        assert!(!text.contains(&p.secret));
        assert!(!text.contains(&p.order));
        assert!(!text.contains(&p.webhook));
    }
    assert_eq!(
        w.snapshot(),
        before,
        "rejected request changed durable state"
    );
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn permission_matrix_rejects_credentials_without_data_or_writes(route in 0usize..25,engine_kind in 0u8..5,credential in 0u8..8,noise in "[ -~]{0,128}",worker in any::<bool>()) {
        runtime().block_on(async {
            let mut w=World::new(3,worker).await;
            w.rotate(0).await;
            w.disable(2).await;
            let catalog=routes(&w.principals[0]);
            let route=&catalog[route];
            let engine=match engine_kind {0=>None,1=>Some("wrong-engine"),2=>Some(w.principals[0].secret.as_str()),3=>Some(w.principals[0].public.as_str()),_=>Some(TEST_ENGINE_TOKEN)};
            // Global routes need only the engine capability. Supply an invalid
            // engine there rather than inventing a tenant requirement.
            let engine=if !route.tenant && engine==Some(TEST_ENGINE_TOKEN) {Some("wrong-engine")} else {engine};
            let auth=w.invalid_auth(credential,&noise);
            denied(&w,route,engine,auth.as_deref(),&serde_json::json!({"tenant_id":w.principals[1].id,"xmr_amount_piconero":1,"refund_address":"forbidden"})).await;
            let good=w.send("GET","/api/v1/admin/tenant",Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",w.principals[0].secret)),&serde_json::json!({})).await;
            assert_eq!(good.status(),StatusCode::OK,"rejections damaged valid authentication");
        });
    }
}

#[test]
fn every_route_runs_the_engine_and_tenant_rejection_matrix() {
    runtime().block_on(async {
        let mut w = World::new(3, true).await;
        w.rotate(0).await;
        w.disable(2).await;
        for route in routes(&w.principals[0]) {
            for engine in [None, Some("wrong"), Some(w.principals[0].secret.as_str())] {
                denied(
                    &w,
                    &route,
                    engine,
                    Some(&format!("Bearer {}", w.principals[0].secret)),
                    &serde_json::json!({}),
                )
                .await;
            }
            if route.tenant {
                for credential in 0..8 {
                    denied(
                        &w,
                        &route,
                        Some(TEST_ENGINE_TOKEN),
                        w.invalid_auth(credential, "fixed").as_deref(),
                        &serde_json::json!({}),
                    )
                    .await;
                }
            }
        }
        // The engine token alone grants instance administration, independently
        // of any tenant bearer. The router here has no injected token layer.
        for auth in [None, Some("Bearer invalid"), Some("Basic invalid")] {
            assert_eq!(
                w.send(
                    "GET",
                    "/status",
                    Some(TEST_ENGINE_TOKEN),
                    auth,
                    &serde_json::json!({})
                )
                .await
                .status(),
                StatusCode::OK
            );
        }
    });
}

async fn ownership_history(count: usize, worker: bool, events: Vec<(u8, usize, usize, bool, u8)>) {
    let mut w = World::new(count, worker).await;
    for (action, actor, target, engine_ok, credential) in events {
        let actor = actor % count;
        let target = target % count;
        if action == 3 {
            if w.principals[actor].enabled {
                w.rotate(actor).await;
            }
            continue;
        }
        if action == 4 {
            if w.principals[actor].enabled {
                w.disable(actor).await;
            }
            continue;
        }
        let p = &w.principals[actor];
        let auth = match credential {
            0 => Some(format!("Bearer {}", p.secret)),
            1 => Some(format!("Bearer {}", p.public)),
            2 => p.old.last().map(|s| format!("Bearer {s}")),
            _ => None,
        };
        let valid = engine_ok && credential == 0 && p.enabled;
        let owner = valid && actor == target;
        let principal = &w.principals[target];
        let (method, uri) = match action {
            0 => (
                "GET",
                format!("/api/v1/admin/tenant/orders/{}", principal.order),
            ),
            1 => (
                "POST",
                format!(
                    "/api/v1/admin/tenant/orders/{}/refund-address",
                    principal.order
                ),
            ),
            _ => ("GET", "/api/v1/admin/tenant/orders".into()),
        };
        let before = w.snapshot();
        let response=w.send(method,&uri,engine_ok.then_some(TEST_ENGINE_TOKEN),auth.as_deref(),&serde_json::json!({"refund_address":"permitted-only-for-owner","tenant_id":principal.id})).await;
        let expected = if !valid {
            StatusCode::UNAUTHORIZED
        } else if action == 2 || owner {
            StatusCode::OK
        } else {
            StatusCode::NOT_FOUND
        };
        assert_eq!(
            response.status(),
            expected,
            "BOUNDARY: tenant-authorization"
        );
        let body = if action == 1 && owner {
            serde_json::Value::Null
        } else {
            body_json(response).await
        };
        if !valid || (action != 2 && !owner) {
            assert_eq!(w.snapshot(), before);
        }
        if action == 2 && valid {
            let list = body.as_array().unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0]["order_id"], w.principals[actor].order);
        }
        if action == 0 && owner {
            assert_eq!(body["order_id"], principal.order);
        }
        if !owner && action != 2 {
            assert!(!body.to_string().contains(&principal.order));
        }
    }
    for p in &w.principals {
        let response = w
            .send(
                "GET",
                "/api/v1/admin/tenant",
                Some(TEST_ENGINE_TOKEN),
                Some(&format!("Bearer {}", p.secret)),
                &serde_json::json!({}),
            )
            .await;
        assert_eq!(
            response.status(),
            if p.enabled {
                StatusCode::OK
            } else {
                StatusCode::UNAUTHORIZED
            }
        );
        for old in &p.old {
            assert_eq!(
                w.send(
                    "GET",
                    "/api/v1/admin/tenant",
                    Some(TEST_ENGINE_TOKEN),
                    Some(&format!("Bearer {old}")),
                    &serde_json::json!({})
                )
                .await
                .status(),
                StatusCode::UNAUTHORIZED
            );
        }
    }
}
proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn tenant_ownership_and_revocation_histories(count in 2usize..5,worker in any::<bool>(),events in prop::collection::vec((0u8..5,0usize..4,0usize..4,any::<bool>(),0u8..4),1..33)) {
        runtime().block_on(ownership_history(count,worker,events));
    }
    #[test]
    fn cross_tenant_webhook_deletion_and_refund_writes_are_atomic(actor in 0usize..4,count in 2usize..5,worker in any::<bool>()) {
        runtime().block_on(async {
            let w=World::new(count,worker).await;
            let actor=actor%count;let target=(actor+1)%count;
            let before=w.snapshot();
            for (method,uri,body) in [
                ("DELETE",format!("/api/v1/admin/tenant/webhooks/{}",w.principals[target].webhook),serde_json::json!({"tenant_id":w.principals[target].id})),
                ("POST",format!("/api/v1/admin/tenant/orders/{}/refund-address",w.principals[target].order),serde_json::json!({"refund_address":"forbidden","tenant_id":w.principals[target].id})),
            ] {
                let response=w.send(method,&uri,Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",w.principals[actor].secret)),&body).await;
                assert_eq!(response.status(),StatusCode::NOT_FOUND);assert_eq!(w.snapshot(),before);
            }
            let response=w.send("DELETE",&format!("/api/v1/admin/tenant/webhooks/{}",w.principals[target].webhook),Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",w.principals[target].secret)),&serde_json::json!({})).await;
            assert_eq!(response.status(),StatusCode::NO_CONTENT,"owner must retain legitimate access");
        });
    }
    #[test]
    fn body_identity_cannot_steer_own_tenant_operations(count in 2usize..5,actor in 0usize..4,amount in 1u64..10000,worker in any::<bool>()) {
        runtime().block_on(async {
            let w=World::new(count,worker).await;
            let actor=actor%count;let target=(actor+1)%count;
            let response=w.send("POST","/api/v1/admin/tenant/orders",Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",w.principals[actor].secret)),&serde_json::json!({"xmr_amount_piconero":amount,"tenant_id":w.principals[target].id,"public_key":w.principals[target].public})).await;
            assert_eq!(response.status(),StatusCode::OK);
            let id=OrderId::new(body_json(response).await["order_id"].as_str().unwrap());
            let store=w.store.lock();
            assert!(store.get_order(&w.principals[actor].id,&id).unwrap().is_some());
            assert!(store.get_order(&w.principals[target].id,&id).unwrap().is_none());
            for (i,p) in w.principals.iter().enumerate() {assert_eq!(store.list_orders(&p.id,None,1000,None).unwrap().len(),1+usize::from(i==actor));}
        });
    }
    #[test]
    fn concurrent_rejections_leave_no_writes_or_stream_slot_leaks(callers in 2usize..33,events in any::<bool>()) {
        runtime().block_on(async {
            let w=World::new(2,true).await;
            let before=w.snapshot();
            let jobs=(0..callers).map(async |i| {
                if events {w.send("GET","/api/v1/admin/tenant/events",Some(TEST_ENGINE_TOKEN),Some("Bearer invalid"),&serde_json::json!({})).await}
                else {w.send("POST",&format!("/api/v1/admin/tenant/orders/{}/refund-address",w.principals[0].order),Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",w.principals[1].secret)),&serde_json::json!({"refund_address":format!("forbidden-{i}")})).await}
            });
            for response in futures_util::future::join_all(jobs).await {assert_eq!(response.status(),if events {StatusCode::UNAUTHORIZED} else {StatusCode::NOT_FOUND});}
            assert_eq!(w.snapshot(),before);
            for _ in 0..2 {
                let response=w.send("GET","/api/v1/admin/tenant/events",Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",w.principals[0].secret)),&serde_json::json!({})).await;
                assert_eq!(response.status(),StatusCode::OK);drop(response);
            }
        });
    }
}

#[test]
fn named_revocation_and_cross_tenant_history_replays() {
    runtime().block_on(ownership_history(
        2,
        true,
        vec![
            (0, 1, 0, true, 0),
            (1, 1, 0, true, 0),
            (3, 0, 0, true, 0),
            (0, 0, 0, true, 2),
            (0, 0, 0, true, 0),
            (4, 0, 0, true, 0),
            (0, 0, 0, true, 0),
            (0, 1, 1, true, 0),
        ],
    ));
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn repeated_headers_bind_only_the_first_presented_capability(actor in 0usize..4,count in 2usize..5,engine_first_valid in any::<bool>(),bearer_first_valid in any::<bool>(),worker in any::<bool>()) {
        runtime().block_on(async {
            let w=World::new(count,worker).await;
            let actor=actor%count;
            let other=(actor+1)%count;
            let first_engine=if engine_first_valid {TEST_ENGINE_TOKEN} else {"invalid-engine"};
            let second_engine=if engine_first_valid {"invalid-engine"} else {TEST_ENGINE_TOKEN};
            let first_auth=if bearer_first_valid {format!("Bearer {}",w.principals[actor].secret)} else {"Basic invalid".into()};
            let before=w.snapshot();
            let mut request=json_request("GET","/api/v1/admin/tenant/orders",None,&serde_json::json!({}));
            request.headers_mut().append(shared::auth::ENGINE_TOKEN_HEADER,first_engine.parse().unwrap());
            request.headers_mut().append(shared::auth::ENGINE_TOKEN_HEADER,second_engine.parse().unwrap());
            request.headers_mut().append("authorization",first_auth.parse().unwrap());
            request.headers_mut().append("authorization",format!("Bearer {}",w.principals[other].secret).parse().unwrap());
            let response=tokio::time::timeout(std::time::Duration::from_secs(5),w.router.clone().oneshot(request)).await.unwrap().unwrap();
            if engine_first_valid && bearer_first_valid {
                assert_eq!(response.status(),StatusCode::OK);
                let body=body_json(response).await;
                assert_eq!(body.as_array().unwrap().len(),1);
                assert_eq!(body[0]["order_id"],w.principals[actor].order);
            } else {assert_eq!(response.status(),StatusCode::UNAUTHORIZED);}
            assert_eq!(w.snapshot(),before);
        });
    }
    #[test]
    fn bearer_syntax_cannot_expand_tenant_authority(kind in 0u8..8,worker in any::<bool>()) {
        runtime().block_on(async {
            let w=World::new(2,worker).await;
            let secret=&w.principals[0].secret;
            let malformed=match kind {
                0=>format!("bearer {secret}"),
                1=>format!("BEARER {secret}"),
                2=>format!("Bearer  {secret}"),
                3=>format!("Bearer\t{secret}"),
                4=>format!("Bearer {secret} "),
                5=>format!("Bearer {secret}, Bearer {}",w.principals[1].secret),
                6=>format!("Bearer{secret}"),
                _=>"Bearer ".into(),
            };
            denied(&w,&routes(&w.principals[0])[5],Some(TEST_ENGINE_TOKEN),Some(&malformed),&serde_json::json!({})).await;
        });
    }
    #[test]
    fn lists_and_live_events_expose_only_the_authenticated_tenant(count in 2usize..5,actor in 0usize..4,foreign_writes in 1usize..13,worker in any::<bool>()) {
        runtime().block_on(async {
            let w=World::new(count,worker).await;
            let actor=actor%count;
            let other=(actor+1)%count;
            let auth=format!("Bearer {}",w.principals[actor].secret);
            for (uri,key,expected) in [
                ("/api/v1/admin/tenant/orders","order_id",w.principals[actor].order.as_str()),
                ("/api/v1/admin/tenant/webhooks","webhook_id",w.principals[actor].webhook.as_str()),
            ] {
                let response=w.send("GET",uri,Some(TEST_ENGINE_TOKEN),Some(&auth),&serde_json::json!({"tenant_id":w.principals[other].id})).await;
                assert_eq!(response.status(),StatusCode::OK);
                let body=body_json(response).await;
                assert_eq!(body.as_array().unwrap().len(),1);
                assert_eq!(body[0][key],expected);
            }
            for (uri,expected) in [
                (format!("/api/v1/admin/tenant/orders?ids={},{}",w.principals[other].order,w.principals[actor].order),Some(w.principals[actor].order.as_str())),
                (format!("/api/v1/admin/tenant/orders?search={}",w.principals[other].order),None),
            ] {
                let response=w.send("GET",&uri,Some(TEST_ENGINE_TOKEN),Some(&auth),&serde_json::json!({})).await;
                assert_eq!(response.status(),StatusCode::OK);
                let body=body_json(response).await;
                assert_eq!(body.as_array().unwrap().len(),usize::from(expected.is_some()));
                if let Some(expected)=expected {assert_eq!(body[0]["order_id"],expected);}
            }
            let response=w.send("GET","/api/v1/admin/tenant/events",Some(TEST_ENGINE_TOKEN),Some(&auth),&serde_json::json!({})).await;
            assert_eq!(response.status(),StatusCode::OK);
            let mut body=response.into_body();let mut buffer=String::new();
            assert_eq!(next_sse_event(&mut body,&mut buffer).await.0,"ready");
            for i in 0..foreign_writes {
                let response=w.send("POST",&format!("/api/v1/admin/tenant/orders/{}/refund-address",w.principals[other].order),Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",w.principals[other].secret)),&serde_json::json!({"refund_address":format!("foreign-{i}")})).await;
                assert_eq!(response.status(),StatusCode::OK);
            }
            let response=w.send("POST",&format!("/api/v1/admin/tenant/orders/{}/refund-address",w.principals[actor].order),Some(TEST_ENGINE_TOKEN),Some(&auth),&serde_json::json!({"refund_address":"owner-barrier"})).await;
            assert_eq!(response.status(),StatusCode::OK);
            let (event,data)=next_sse_event(&mut body,&mut buffer).await;
            assert_eq!(event,"order");
            assert_eq!(serde_json::from_str::<serde_json::Value>(&data).unwrap()["order_id"],w.principals[actor].order);
            // A positive owner event after the foreign writes makes a missing
            // or broken stream fail instead of passing a silence-only check.
        });
    }
}

proptest! {
    #![proptest_config(persisted_config(config()))]
    #[test]
    fn payment_lookup_cannot_read_or_credit_another_tenants_outputs(count in 2usize..5,foreign in 1usize..4,repeats in 1usize..5,worker in any::<bool>()) {
        use monero::cryptonote::hash::Hashable as _;
        runtime().block_on(async {
            let (node_state,daemon)=test_app_state_with_real_daemon().await;
            let w=World::with_network(count,worker,Some(node_state)).await;
            let foreign=1+(foreign-1)%(count-1);
            let tx=fixture_tx_for_lookup_tests();
            let txid=hex::encode(tx.hash().to_bytes());
            daemon.set_mempool(vec![tx]);
            let owner=&w.principals[0];
            let foreign=&w.principals[foreign];
            for owner_has_looked_up in [false,true] {
                if owner_has_looked_up {
                    let response=w.send("POST","/api/v1/admin/tenant/payments/lookup",Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",owner.secret)),&serde_json::json!({"txid":txid})).await;
                    assert_eq!(response.status(),StatusCode::OK);
                    let body=body_json(response).await;
                    assert_eq!(body["outcome"],"matched");
                    assert_eq!(body["order_ids"],serde_json::json!([owner.order]));
                    assert_eq!(w.store.lock().get_all_payments(&OrderId::new(&owner.order)).unwrap().len(),1);
                }
                let before=w.snapshot();
                for _ in 0..repeats {
                    let response=w.send("POST","/api/v1/admin/tenant/payments/lookup",Some(TEST_ENGINE_TOKEN),Some(&format!("Bearer {}",foreign.secret)),&serde_json::json!({"txid":txid,"tenant_id":owner.id,"order_id":owner.order,"public_key":owner.public})).await;
                    assert_eq!(response.status(),StatusCode::OK);
                    let body=body_json(response).await;
                    assert_eq!(body["outcome"],"no_matching_order");
                    assert!(!body.to_string().contains(&owner.order));
                    assert_eq!(w.snapshot(),before,"foreign lookup mutated payment state");
                    assert!(w.store.lock().get_all_payments(&OrderId::new(&foreign.order)).unwrap().is_empty());
                }
            }
        });
    }
}

fn persisted_config(config: proptest::test_runner::Config) -> proptest::test_runner::Config {
    crate::property_support::persist(
        config,
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/proptest-regressions/http/authorization_properties.txt"
        ),
    )
}
