//! Stateful handle ownership and real backend replacement histories.
use super::*;
use crate::property_support::{config, custody_arc, runtime, GateCustody};
use proptest::prelude::*;
use std::sync::atomic::Ordering;

fn material(seed: u8) -> WalletMaterial {
    let mut raw = [seed; 32];
    raw[31] = 0;
    let key = monero::PrivateKey::from_slice(&raw).unwrap();
    WalletMaterial::new(raw, monero::PublicKey::from_private_key(&key).to_bytes())
}
fn expected(seed: u8, index: SubaddressIndex, network: Network) -> Address {
    let pair = material(seed).to_view_pair().unwrap();
    if index.is_zero() {
        Address::standard(
            network,
            pair.spend,
            monero::PublicKey::from_private_key(&pair.view),
        )
    } else {
        monero::cryptonote::subaddress::get_subaddress(&pair, index, Some(network))
    }
}
struct OwnedHandle {
    handle: WalletHandle,
    backend: usize,
    seed: u8,
    live: bool,
}
async fn assert_model(
    router: &CustodyRouter,
    backends: &[Arc<GateCustody>; 2],
    handles: &[OwnedHandle],
    index: SubaddressIndex,
) {
    for h in handles {
        let answer = router
            .derive_subaddress(h.handle, index, Network::Mainnet)
            .await;
        if h.live {
            assert_eq!(answer.unwrap(), expected(h.seed, index, Network::Mainnet));
            assert_eq!(
                router.backend_of(h.handle).as_deref(),
                Some(if h.backend == 0 { "a" } else { "b" })
            );
        } else {
            assert!(matches!(answer, Err(KeyCustodyError::UnknownWallet)));
            assert!(!router.handle_is_live(h.handle));
        }
    }
    for (i, c) in backends.iter().enumerate() {
        assert_eq!(
            c.inner.wallet_count(),
            handles.iter().filter(|h| h.live && h.backend == i).count()
        );
    }
}
async fn wait_empty(c: &GateCustody) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while c.inner.wallet_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn registration_removal_and_backend_reload_histories_never_cross_wallets(
        events in proptest::collection::vec((0u8..8,0usize..2,0u8..4),1..25),minor in any::<u32>(),
    ) {
        runtime().block_on(async {
            let mut backends=[Arc::new(GateCustody::default()),Arc::new(GateCustody::default())];
            let router=CustodyRouter::new(HashMap::from([("a".to_owned(),custody_arc(&backends[0])),("b".to_owned(),custody_arc(&backends[1]))]),"a");
            let mut enabled=[true,true];let mut handles=Vec::<OwnedHandle>::new();
            // Both backends always begin with a live wallet, so isolation
            // assertions cannot pass merely because one side is empty.
            for i in 0..2 {let h=router.register_wallet_in(if i==0 {"a"} else {"b"},material(i as u8+1)).await.unwrap();handles.push(OwnedHandle {handle:h,backend:i,seed:i as u8+1,live:true});}
            for (op,backend,wallet) in events {
                let name=if backend==0 {"a"} else {"b"};let seed=wallet+3;
                match op {
                    0|1=>{
                        let result=if op==0 {router.register_wallet_in(name,material(seed)).await} else {
                            let sealed=backends[backend].inner.seal(&material(seed)).await.unwrap();
                            router.unseal_and_register_in_idempotent(name,&sealed,&format!("wallet-{wallet}")).await
                        };
                        if enabled[backend] {let h=result.unwrap();if !handles.iter().any(|old|old.handle==h&&old.live) {handles.push(OwnedHandle {handle:h,backend,seed,live:true});}}
                        else {assert!(matches!(result,Err(KeyCustodyError::BackendUnavailable(_))));}
                    }
                    2|3=>{
                        let old=Arc::clone(&backends[backend]);let mut map=router.backends();
                        if op==2 {backends[backend]=Arc::new(GateCustody::default());map.insert(name.to_owned(),custody_arc(&backends[backend]));enabled[backend]=true;}
                        else {map.remove(name);enabled[backend]=false;}
                        let dropped=router.replace(map,"a");let expected=handles.iter().filter(|h|h.live&&h.backend==backend).count();assert_eq!(dropped.len(),expected);
                        for h in &mut handles {if h.backend==backend {h.live=false;}}
                        free_handles(dropped);wait_empty(&old).await;
                    }
                    4|5=>{
                        if !handles.is_empty() {let at=usize::from(wallet)%handles.len();let h=&mut handles[at];
                            if h.live {
                                if op==4 {router.remove_wallet(h.handle).await.unwrap();}
                                else {backends[h.backend].inner.remove_wallet(h.handle).await.unwrap();assert!(matches!(router.derive_subaddress(h.handle,SubaddressIndex::default(),Network::Mainnet).await,Err(KeyCustodyError::UnknownWallet)));}
                                h.live=false;
                            } else {assert!(matches!(router.remove_wallet(h.handle).await,Err(KeyCustodyError::UnknownWallet)));}
                        }
                    }
                    6=>{assert!(router.replace(router.backends(),"a").is_empty());}
                    _=>{assert!(matches!(router.derive_subaddress(WalletHandle::generate(),SubaddressIndex::default(),Network::Mainnet).await,Err(KeyCustodyError::UnknownWallet)));}
                }
                assert_model(&router,&backends,&handles,SubaddressIndex {major:0,minor}).await;
            }
        });
    }
    #[test]
    fn transient_backend_failures_preserve_ownership_until_recovery(calls in 1usize..9,minor in any::<u32>(),network in 0u8..3) {
        runtime().block_on(async {
            let c=Arc::new(GateCustody::default());let router=CustodyRouter::new(HashMap::from([("a".to_owned(),custody_arc(&c))]),"a");let h=router.register_wallet(material(3)).await.unwrap();
            let network=match network {0=>Network::Mainnet,1=>Network::Testnet,_=>Network::Stagenet};let index=SubaddressIndex {major:0,minor};c.mode.store(1,Ordering::Relaxed);
            for _ in 0..calls {assert!(matches!(router.derive_subaddress(h,index,network).await,Err(KeyCustodyError::BackendUnavailable(_))));assert!(router.handle_is_live(h));}
            c.mode.store(0,Ordering::Relaxed);assert_eq!(router.derive_subaddress(h,index,network).await.unwrap(),expected(3,index,network));assert_eq!(c.inner.wallet_count(),1);
            assert!(c.attempted.load(Ordering::Relaxed)>calls);
        });
    }
    #[test]
    fn backend_epoch_changes_invalidate_only_the_restarted_backend(which in 0usize..2,epoch in 1u64..=u64::MAX) {
        runtime().block_on(async {
            let backends=[Arc::new(GateCustody::default()),Arc::new(GateCustody::default())];
            let router=CustodyRouter::new(HashMap::from([("a".to_owned(),custody_arc(&backends[0])),("b".to_owned(),custody_arc(&backends[1]))]),"a");
            router.check_state().await.unwrap();let a=router.register_wallet_in("a",material(1)).await.unwrap();let b=router.register_wallet_in("b",material(2)).await.unwrap();let hs=[a,b];
            backends[which].epoch.store(epoch,Ordering::Relaxed);router.check_state().await.unwrap();wait_empty(&backends[which]).await;
            assert!(!router.handle_is_live(hs[which]));assert!(router.handle_is_live(hs[1-which]));assert_eq!(backends[1-which].inner.wallet_count(),1);
            let new=router.register_wallet_in(if which==0 {"a"} else {"b"},material(3)).await.unwrap();router.check_state().await.unwrap();assert!(router.handle_is_live(new),"unchanged epoch invalidated a fresh registration");
            backends[1-which].epoch.store(u64::MAX,Ordering::Relaxed);assert_eq!(router.check_state().await.unwrap(),u64::MAX,"combined epochs must not overflow");
        });
    }
    #[test]
    fn registrations_finishing_after_replacement_are_freed_and_retry_on_the_new_backend(count in 1usize..9) {
        runtime().block_on(async {
            let old=Arc::new(GateCustody::default());old.registration_mode.store(2,Ordering::Relaxed);
            let router=Arc::new(CustodyRouter::new(HashMap::from([("a".to_owned(),custody_arc(&old))]),"a"));
            let mut tasks=vec![];
            for i in 0..count {let r=Arc::clone(&router);tasks.push(tokio::spawn(async move {r.register_wallet(material(i as u8+1)).await}));}
            tokio::time::timeout(std::time::Duration::from_secs(5),async {while old.registrations.load(Ordering::Relaxed)<count {tokio::task::yield_now().await;}}).await.unwrap();assert_eq!(old.inner.wallet_count(),count);
            let new=Arc::new(GateCustody::default());assert!(router.replace(HashMap::from([("a".to_owned(),custody_arc(&new))]),"a").is_empty());old.registration_release.notify_waiters();
            for task in tasks {assert!(matches!(task.await.unwrap(),Err(KeyCustodyError::BackendUnavailable(_))));}
            wait_empty(&old).await;
            for i in 0..count {let h=router.register_wallet(material(i as u8+1)).await.unwrap();assert_eq!(router.derive_subaddress(h,SubaddressIndex::default(),Network::Mainnet).await.unwrap(),expected(i as u8+1,SubaddressIndex::default(),Network::Mainnet));}
            assert_eq!(new.inner.wallet_count(),count);
        });
    }
    #[test]
    fn cancelled_registration_results_are_recovered_by_the_same_registration_id(seed in 1u8..100,key in "[a-z0-9]{1,128}") {
        runtime().block_on(async {
            let c=Arc::new(GateCustody::default());let sealed=c.inner.seal(&material(seed)).await.unwrap();c.registration_mode.store(2,Ordering::Relaxed);
            let router=Arc::new(CustodyRouter::new(HashMap::from([("a".to_owned(),custody_arc(&c))]),"a"));let r=Arc::clone(&router);let bytes=sealed.clone();let id=key.clone();
            let task=tokio::spawn(async move {r.unseal_and_register_in_idempotent("a",&bytes,&id).await});
            tokio::time::timeout(std::time::Duration::from_secs(5),c.registration_entered.notified()).await.unwrap();assert_eq!(c.inner.wallet_count(),1);task.abort();assert!(task.await.unwrap_err().is_cancelled());
            c.registration_mode.store(0,Ordering::Relaxed);let h=router.unseal_and_register_in_idempotent("a",&sealed,&key).await.unwrap();assert_eq!(c.inner.wallet_count(),1);
            assert_eq!(router.derive_subaddress(h,SubaddressIndex::default(),Network::Mainnet).await.unwrap(),expected(seed,SubaddressIndex::default(),Network::Mainnet));
            assert_eq!(router.unseal_and_register_in_idempotent("a",&sealed,&key).await.unwrap(),h);router.remove_wallet(h).await.unwrap();assert_eq!(c.inner.wallet_count(),0);
        });
    }
}

#[test]
fn two_maximum_backend_epochs_do_not_panic_or_wrap() {
    runtime().block_on(async {
        let a = Arc::new(GateCustody::default());
        let b = Arc::new(GateCustody::default());
        a.epoch.store(u64::MAX, Ordering::Relaxed);
        b.epoch.store(u64::MAX, Ordering::Relaxed);
        let router = CustodyRouter::new(
            HashMap::from([
                ("a".to_owned(), custody_arc(&a)),
                ("b".to_owned(), custody_arc(&b)),
            ]),
            "a",
        );
        assert_eq!(router.check_state().await.unwrap(), u64::MAX);
    });
}
