//! Generated crypto-boundary histories, with a real paying transaction.
use super::*;
use crate::property_support::{config, runtime};
use monero::Transaction;
use proptest::prelude::*;
use std::collections::BTreeSet;
use std::future::Future as _;

fn fixture() -> (WalletMaterial, Transaction) {
    let view = hex::decode("bcfdda53205318e1c14fa0ddca1a45df363bb427972981d0249d0f4652a7df07")
        .unwrap()
        .try_into()
        .unwrap();
    let spend = PrivateKey::from_slice(
        &hex::decode("e5f4301d32f3bdaef814a835a18aaaa24b13cc76cf01a832a7852faf9322e907").unwrap(),
    )
    .unwrap();
    let tx = monero::consensus::encode::deserialize(
        &hex::decode(include_str!("../../tests/fixtures/subaddress_tx.hex")).unwrap(),
    )
    .unwrap();
    (
        WalletMaterial::new(view, PublicKey::from_private_key(&spend).to_bytes()),
        tx,
    )
}
type OutputRow = (usize, u32, u32, Option<u64>);
type BatchRows = Vec<(usize, Vec<OutputRow>)>;
fn outputs(v: &[MatchedOutput]) -> Vec<OutputRow> {
    v.iter()
        .map(|o| {
            (
                o.output_index,
                o.subaddress_index.major,
                o.subaddress_index.minor,
                o.amount_piconero,
            )
        })
        .collect()
}
fn batch(v: &[TxMatches]) -> BatchRows {
    v.iter().map(|tx| (tx.tx, outputs(&tx.outputs))).collect()
}
fn derivations(c: &PlainKeyCustody, h: WalletHandle) -> u64 {
    c.wallets
        .read()
        .get(&h)
        .unwrap()
        .derivations
        .load(Ordering::Relaxed)
}
fn minor() -> impl Strategy<Value = u32> {
    prop_oneof![0u32..16, any::<u32>(), Just(u32::MAX)]
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn sparse_window_histories_match_fresh_scans_without_rebuilding_kept_keys(
        windows in proptest::collection::vec(proptest::collection::vec(minor(),0..13),1..13),
        paying in proptest::collection::vec(any::<bool>(),1..9),
    ) {
        runtime().block_on(async {
            let (material,tx)=fixture();let c=PlainKeyCustody::default();let h=c.register_wallet(material).await.unwrap();
            let mut other=tx.clone();other.prefix.outputs.reverse();
            let txs:Vec<_>=paying.iter().map(|p|ScanInput::of(if *p {&tx} else {&other})).collect();
            let golden=c.scan_tx_outputs(h,&ScanInput::of(&tx),0..1,0..3).await.unwrap();
            assert_eq!(golden.len(),1);assert_eq!(golden[0].output_index,1);assert!(golden[0].amount_piconero.unwrap()>0);
            assert!(c.scan_tx_outputs(h,&ScanInput::of(&other),0..1,0..3).await.unwrap().is_empty());
            let mut previous=BTreeSet::new();
            // Always exercise matching, removing, then rediscovering the payment.
            let windows=[vec![1],vec![],vec![1]].into_iter().chain(windows);
            for indices in windows {
                let wanted:BTreeSet<_>=indices.iter().copied().collect();let window=ScanIndices::new(indices);
                let before=derivations(&c,h);let found=c.scan_txs_for_indices(h,&txs,&window).await.unwrap();
                assert_eq!(derivations(&c,h)-before,wanted.difference(&previous).count() as u64);
                let fresh=PlainKeyCustody::default();let fh=fresh.register_wallet(fixture().0).await.unwrap();
                let expected=fresh.scan_txs_for_indices(fh,&txs,&window).await.unwrap();assert_eq!(batch(&found),batch(&expected));
                let oracle:Vec<_>=paying.iter().enumerate().filter(|(_,p)|**p && wanted.contains(&1)).map(|(i,_)|(i,outputs(&golden))).collect();
                assert_eq!(batch(&found),oracle,"cache or batch positions changed actual money");
                let before=derivations(&c,h);assert_eq!(batch(&c.scan_txs_for_indices(h,&txs,&window).await.unwrap()),oracle);
                assert_eq!(derivations(&c,h),before,"unchanged windows re-derived keys");previous=wanted;
            }
        });
    }
    #[test]
    fn lookup_ranges_match_fresh_scans_and_do_not_poison_the_live_window(
        ranges in proptest::collection::vec((0u32..4,0u32..4,0u32..8,0u32..8),1..13),high in any::<bool>(),
    ) {
        runtime().block_on(async {
            let (material,tx)=fixture();let c=PlainKeyCustody::default();let h=c.register_wallet(material).await.unwrap();let input=ScanInput::of(&tx);let live=ScanIndices::new([1,5]);
            let golden=c.scan_txs_for_indices(h,std::slice::from_ref(&input),&live).await.unwrap();assert_eq!(golden.len(),1);
            for (a,b,x,y) in ranges {
                let (major,minor)=if high {(u32::MAX-a..u32::MAX-b,u32::MAX-x..u32::MAX-y)} else {(a..b,x..y)};
                let fresh=PlainKeyCustody::default();let fh=fresh.register_wallet(fixture().0).await.unwrap();
                let found=c.scan_tx_outputs(h,&input,major.clone(),minor.clone()).await.unwrap();
                assert_eq!(outputs(&found),outputs(&fresh.scan_tx_outputs(fh,&input,major.clone(),minor.clone()).await.unwrap()));
                assert_eq!(found.len(),usize::from(major.contains(&0)&&minor.contains(&1)));
                let before=derivations(&c,h);c.scan_tx_outputs(h,&input,major,minor).await.unwrap();assert_eq!(derivations(&c,h),before);
                assert_eq!(batch(&c.scan_txs_for_indices(h,std::slice::from_ref(&input),&live).await.unwrap()),batch(&golden));
                assert_eq!(derivations(&c,h),before,"lookup invalidated the live table");
            }
        });
    }
    #[test]
    fn cancelling_queued_scans_preserves_both_caches(lookup in any::<bool>(),warm in any::<bool>(),extras in proptest::collection::vec(2u32..20,0..12)) {
        let rt=tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
        rt.block_on(async {
            let (material,tx)=fixture();let c=PlainKeyCustody::default();let h=c.register_wallet(material).await.unwrap();let input=ScanInput::of(&tx);
            let window=ScanIndices::new(std::iter::once(1).chain(extras));
            if warm {c.scan_txs_for_indices(h,std::slice::from_ref(&input),&window).await.unwrap();c.scan_tx_outputs(h,&input,0..1,0..20).await.unwrap();}
            let (release,wait)=std::sync::mpsc::channel::<()>();let (entered,ready)=tokio::sync::oneshot::channel();
            let blocker=tokio::task::spawn_blocking(move || {entered.send(()).unwrap();let _=wait.recv();});ready.await.unwrap();
            {
                let mut scan=std::pin::pin!(async {
                    if lookup {c.scan_tx_outputs(h,&input,0..1,0..20).await.unwrap();} else {c.scan_txs_for_indices(h,std::slice::from_ref(&input),&window).await.unwrap();}
                });
                std::future::poll_fn(|cx| {assert!(scan.as_mut().poll(cx).is_pending(),"cancellation did not reach blocked CPU work");std::task::Poll::Ready(())}).await;
            }
            drop(release);blocker.await.unwrap();
            let recovered=c.scan_txs_for_indices(h,std::slice::from_ref(&input),&window).await.unwrap();assert_eq!(recovered.len(),1);assert_eq!(recovered[0].outputs[0].output_index,1);
            assert_eq!(c.scan_tx_outputs(h,&input,0..1,0..20).await.unwrap(),recovered[0].outputs);
        });
    }
    #[test]
    fn idempotent_registration_sealing_and_removal_preserve_wallet_identity(
        count in 1usize..17,key in "[a-z0-9]{1,128}",major in any::<u32>(),minor in any::<u32>(),network in 0u8..3,
    ) {
        runtime().block_on(async {
            let c=PlainKeyCustody::default();let material=fixture().0;let pair=material.to_view_pair().unwrap();let sealed=c.seal(&material).await.unwrap();
            let handles=futures_util::future::join_all(std::iter::repeat_with(||c.unseal_and_register_idempotent(&sealed,&key)).take(count)).await;
            let h=handles[0].as_ref().copied().unwrap();assert!(handles.into_iter().all(|r|r.unwrap()==h));assert_eq!(c.wallet_count(),1);
            let network=match network {0=>Network::Mainnet,1=>Network::Testnet,_=>Network::Stagenet};let index=SubaddressIndex {major,minor};
            let expected=if index.is_zero() {Address::standard(network,pair.spend,PublicKey::from_private_key(&pair.view))} else {monero::cryptonote::subaddress::get_subaddress(&pair,index,Some(network))};
            assert_eq!(c.derive_subaddress(h,index,network).await.unwrap(),expected);
            let mut other=[7;32];other[31]=0;let other=WalletMaterial::new(other,PublicKey::from_private_key(&PrivateKey::from_slice(&other).unwrap()).to_bytes());let other=c.seal(&other).await.unwrap();
            assert!(matches!(c.unseal_and_register_idempotent(&other,&key).await,Err(KeyCustodyError::InvalidKeyMaterial(_))));assert_eq!(c.wallet_count(),1);
            c.remove_wallet(h).await.unwrap();assert!(matches!(c.derive_subaddress(h,index,network).await,Err(KeyCustodyError::UnknownWallet)));assert_eq!(c.wallet_count(),0);
            let next=c.unseal_and_register_idempotent(&sealed,&key).await.unwrap();assert_ne!(h,next);assert_eq!(c.derive_subaddress(next,index,network).await.unwrap(),expected);
            let restarted=PlainKeyCustody::default();let rh=restarted.unseal_and_register(&sealed).await.unwrap();assert_eq!(restarted.derive_subaddress(rh,index,network).await.unwrap(),expected);
        });
    }
    #[test]
    fn invalid_material_and_oversized_ranges_cannot_poison_a_live_wallet(
        bytes in proptest::collection::vec(any::<u8>(),0..97),entries in 1_000_001u32..=u32::MAX,
    ) {
        runtime().block_on(async {
            let c=PlainKeyCustody::default();let (material,tx)=fixture();let h=c.register_wallet(material).await.unwrap();let before=c.wallet_count();
            let valid=WalletMaterial::from_raw_bytes(&bytes).and_then(|m|m.to_view_pair());
            let result=c.unseal_and_register(&bytes).await;
            if let Ok(pair)=valid {let registered=result.unwrap();assert_eq!(c.wallet_count(),before+1);assert_eq!(c.derive_subaddress(registered,SubaddressIndex::default(),Network::Mainnet).await.unwrap(),Address::standard(Network::Mainnet,pair.spend,PublicKey::from_private_key(&pair.view)));c.remove_wallet(registered).await.unwrap();} else {assert!(matches!(result,Err(KeyCustodyError::InvalidKeyMaterial(_))));assert_eq!(c.wallet_count(),before);}
            let input=ScanInput::of(&tx);assert!(matches!(c.scan_tx_outputs(h,&input,0..1,0..entries).await,Err(KeyCustodyError::ScanFailed(_))));
            assert_eq!(c.scan_txs_for_indices(h,&[input],&ScanIndices::new([1])).await.unwrap().len(),1);
        });
    }
}

#[test]
fn table_build_batches_keep_exact_progress_across_window_changes() {
    let pair = fixture().0.to_view_pair().unwrap();
    let mut table = KeyTable::default();
    for count in [255, 256, 257, 511, 512, 513] {
        let window = ScanIndices::new(0..count);
        let before = table.indices.clone();
        let expected = window
            .minors()
            .iter()
            .filter(|i| !before.contains(i))
            .count();
        let mut derived = 0;
        while table.covers.as_ref() != Some(&window) {
            let n = table.update_batch_to(&pair, &window);
            assert!(n <= SCAN_TABLE_BUILD_BATCH as u64);
            derived += n;
            assert!(table.pending_indices.len() < count as usize);
        }
        assert_eq!(derived, expected as u64);
        assert_eq!(table.table.len(), count as usize);
        assert_eq!(table.update_batch_to(&pair, &window), 0);
    }
    let small = ScanIndices::new([1, 7]);
    assert_eq!(table.update_batch_to(&pair, &small), 0);
    assert_eq!(table.table.len(), 2);
    let changing = ScanIndices::new(0..513);
    table.update_batch_to(&pair, &changing);
    let wanted = ScanIndices::new([1, 7, u32::MAX]);
    while table.covers.as_ref() != Some(&wanted) {
        table.update_batch_to(&pair, &wanted);
    }
    assert_eq!(table.indices, wanted.minors().iter().copied().collect());
    assert_eq!(table.table.len(), 3);
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn concurrent_wallet_scans_keep_independent_caches_and_matches(
        windows in proptest::collection::vec((any::<bool>(),proptest::collection::vec(0u32..12,0..12)),2..17),
    ) {
        runtime().block_on(async {
            let (material,tx)=fixture();let c=PlainKeyCustody::default();let paying=c.register_wallet(material).await.unwrap();
            let mut raw=[3;32];raw[31]=0;let unrelated=c.register_wallet(WalletMaterial::new(raw,PublicKey::from_private_key(&PrivateKey::from_slice(&raw).unwrap()).to_bytes())).await.unwrap();
            let input=ScanInput::of(&tx);let golden=c.scan_tx_outputs(paying,&input,0..1,0..3).await.unwrap();assert_eq!(golden.len(),1);
            assert!(c.scan_tx_outputs(unrelated,&input,0..1,0..3).await.unwrap().is_empty());
            let windows=[(true,vec![1]),(false,vec![1]),(true,vec![]),(true,vec![1])].into_iter().chain(windows);
            futures_util::future::join_all(windows.map(|(is_paying,indices)| {
                let window=ScanIndices::new(indices);let input=&input;let c=&c;let golden=&golden;
                async move {
                    let h=if is_paying {paying} else {unrelated};
                    let result=c.scan_txs_for_indices(h,std::slice::from_ref(input),&window).await.unwrap();
                    let oracle=if is_paying&&window.minors().contains(&1) {vec![(0,outputs(golden))]} else {vec![]};
                    assert_eq!(batch(&result),oracle);
                }
            })).await;
        });
    }
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn invalid_registration_ids_never_allocate_wallets(key in prop_oneof![any::<String>(),Just(String::new()),Just("k".repeat(128)),Just("k".repeat(129))]) {
        runtime().block_on(async {
            let c=PlainKeyCustody::default();let sealed=c.seal(&fixture().0).await.unwrap();let result=c.unseal_and_register_idempotent(&sealed,&key).await;
            if key.is_empty()||key.len()>128 {assert!(matches!(result,Err(KeyCustodyError::InvalidKeyMaterial(_))));assert_eq!(c.wallet_count(),0);}
            else {let h=result.unwrap();assert_eq!(c.wallet_count(),1);assert_eq!(c.unseal_and_register_idempotent(&sealed,&key).await.unwrap(),h);}
        });
    }
}
