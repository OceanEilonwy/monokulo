use super::*;
use abuse::ClientIdentity;

#[test]
fn each_rendezvous_circuit_is_its_own_visitor_as_with_an_external_tor() {
    assert_eq!(circuit_peer(1).identity(), ClientIdentity::Circuit(1));
    assert_eq!(
        circuit_peer(0xaabb_ccdd).identity(),
        ClientIdentity::Circuit(0xaabb_ccdd)
    );
    assert_ne!(circuit_peer(1).identity(), circuit_peer(2).identity());
}

fn inside(state_dir: &Path, hidden_service_dir: Option<PathBuf>) -> Option<Wanted> {
    Some(Wanted {
        state_dir: state_dir.to_path_buf(),
        hidden_service_dir,
    })
}

#[test]
fn starting_makes_a_private_state_folder_and_needs_a_real_hidden_service_dir() {
    let dir = tempfile_dir("tor-prepare");
    let state_dir = dir.join("tor");
    let tor = Tor::default();
    let change = tor.prepare(inside(&state_dir, None)).unwrap();
    assert!(matches!(change, TorChange::Start(_)));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&state_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    let empty = dir.join("not-a-hidden-service");
    std::fs::create_dir_all(&empty).unwrap();
    let refused = tor
        .prepare(inside(&state_dir, Some(empty.clone())))
        .err()
        .unwrap();
    assert_eq!(refused.field, WantedField::HiddenServiceDir);
    // A C tor key, the tag then the 64-byte expanded secret key, in a
    // folder only this user can open, as tor keeps it.
    prepare_state_dir(&empty).unwrap();
    let mut key = b"== ed25519v1-secret: type0 ==\0\0\0".to_vec();
    key.extend([7u8; 64]);
    std::fs::write(empty.join(CTOR_SECRET_KEY_FILE), &key).unwrap();
    let problem = tor
        .prepare(inside(&state_dir, Some(empty.clone())))
        .err()
        .map(|e| e.message);
    assert_eq!(problem, None);

    // Arti made a key of its own in this state folder on an earlier run;
    // the HiddenServiceDir's is still the one served, and a hostname file
    // naming another address is refused.
    #[cfg(feature = "arti")]
    {
        use safelog::DisplayRedacted as _;
        let own = Wanted {
            state_dir: state_dir.clone(),
            hidden_service_dir: None,
        };
        let (config, service) = arti::configs(&own).unwrap();
        let own_address =
            arti_client::TorClient::<tor_rtcompat::PreferredRuntime>::create_onion_service(
                &config, service,
            )
            .unwrap()
            .generate_identity_key(tor_keymgr::KeystoreSelector::Primary)
            .unwrap()
            .display_unredacted()
            .to_string();
        let served = arti::address_to_serve(&Wanted {
            state_dir: state_dir.clone(),
            hidden_service_dir: Some(empty.clone()),
        })
        .unwrap()
        .unwrap();
        assert_ne!(served, own_address);
        let fresh = dir.join("fresh");
        prepare_state_dir(&fresh).unwrap();
        let from_fresh = arti::address_to_serve(&Wanted {
            state_dir: fresh,
            hidden_service_dir: Some(empty.clone()),
        })
        .unwrap();
        assert_eq!(from_fresh.as_ref(), Some(&served), "the key alone decides");

        std::fs::write(empty.join(HOSTNAME_FILE), format!("{own_address}\n")).unwrap();
        let refused = tor
            .prepare(inside(&state_dir, Some(empty.clone())))
            .err()
            .unwrap();
        assert_eq!(refused.field, WantedField::HiddenServiceDir);
        assert!(
            refused.message.contains(&served) && refused.message.contains(&own_address),
            "{}",
            refused.message
        );
        std::fs::write(empty.join(HOSTNAME_FILE), format!("{served}\n")).unwrap();
        assert!(tor.prepare(inside(&state_dir, Some(empty))).is_ok());
    }

    // Off or external: nothing to run.
    let change = tor.prepare(None).unwrap();
    assert!(matches!(change, TorChange::Stop));
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn a_start_asked_for_before_the_router_exists_waits_for_it() {
    let tor = Tor::default();
    let wanted = Wanted {
        state_dir: PathBuf::from("/nonexistent"),
        hidden_service_dir: None,
    };
    tor.start(wanted.clone());
    assert_eq!(tor.running_with(), Some(wanted));
    assert_eq!(
        tor.status(),
        TorStatus::Off,
        "nothing runs until the router is ready"
    );
    tor.stop();
    assert_eq!(tor.running_with(), None);
}

/// A fresh folder for one test.
fn tempfile_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("monokulo-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
