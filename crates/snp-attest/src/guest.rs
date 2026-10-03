//! Asking the AMD security processor (through the guest kernel's
//! `/dev/sev-guest`) for the two things a confidential guest needs from it:
//! an attestation report carrying 64 bytes of its own choosing, and a key
//! derived from the chip's secret and the guest's own launch identity.
//!
//! The derived key is what makes sealing possible: the firmware mixes the
//! chip's root key with the guest fields the request selects, so only a guest
//! with the same values for those fields, on the same chip, gets the same
//! key. Which fields the firmware can mix is fixed by AMD (`FIELD_*`): the ID
//! key that signed the guest's ID block is **not** one of them, so a key
//! bound only to family/image ID and SVN can be derived by any guest whose
//! launcher wrote the same values into an ID block of its own. Binding the
//! measurement closes that; see `key_custody::snp`.
//!
//! [`SevGuest`] is the real device. [`TestGuest`] (feature `test-support`)
//! stands in for it in tests: it signs its reports with a key of its own and
//! derives keys the way the firmware does, from a secret and the selected
//! fields, so tests see the same behaviour (a different measurement gets a
//! different key) without the hardware.
//!
//! Layouts and ioctl numbers are those of Linux's `include/uapi/linux/sev-guest.h`
//! and AMD's SNP firmware ABI (`MSG_REPORT_REQ`/`MSG_KEY_REQ`, response
//! status at 0x00 and payload at 0x20).

use zeroize::Zeroizing;

/// Guest fields a derived key can be bound to (`GUEST_FIELD_SELECT`).
pub const FIELD_POLICY: u64 = 1 << 0;
pub const FIELD_IMAGE_ID: u64 = 1 << 1;
pub const FIELD_FAMILY_ID: u64 = 1 << 2;
pub const FIELD_MEASUREMENT: u64 = 1 << 3;
pub const FIELD_GUEST_SVN: u64 = 1 << 4;
pub const FIELD_TCB_VERSION: u64 = 1 << 5;

/// What a derived key is mixed from, besides the chip's root key (VCEK).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DerivedKeyRequest {
    /// The `FIELD_*` bits to mix in.
    pub guest_field_select: u64,
    /// The privilege level the key is for; 0 is the guest kernel's own.
    pub vmpl: u32,
    /// Mixed in (when selected) instead of the guest's current SVN; must not
    /// exceed it.
    pub guest_svn: u32,
    /// Mixed in (when selected) instead of the current TCB; must not exceed it.
    pub tcb_version: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum GuestError {
    #[error("can't open {path}: {error}")]
    Open { path: String, error: String },
    #[error("the {request} request to the security processor failed: firmware error {fw:#x}, hypervisor error {vmm:#x} ({os})")]
    Request {
        request: &'static str,
        fw: u32,
        vmm: u32,
        os: String,
    },
    #[error("the security processor refused the {request} request (status {status:#x})")]
    Status { request: &'static str, status: u32 },
    #[error("SEV-SNP guest requests are only available on Linux")]
    Unsupported,
}

/// The guest's view of the security processor.
pub trait GuestDevice: Send + Sync {
    /// A signed attestation report (the raw 1184 bytes) with `report_data`
    /// in its REPORT_DATA field, requested at VMPL 0.
    fn report(&self, report_data: &[u8; 64]) -> Result<Vec<u8>, GuestError>;

    /// A 32-byte key derived from the chip's VCEK and the selected fields.
    fn derived_key(&self, request: &DerivedKeyRequest) -> Result<Zeroizing<[u8; 32]>, GuestError>;
}

/// The real device, by path (normally `/dev/sev-guest`). Opened per request:
/// requests are rare (one report per boot, a few keys).
#[cfg(feature = "guest")]
#[derive(Debug, Clone)]
pub struct SevGuest {
    path: std::path::PathBuf,
}

#[cfg(feature = "guest")]
impl SevGuest {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        SevGuest { path: path.into() }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(all(feature = "guest", target_os = "linux"))]
mod linux {
    use super::{DerivedKeyRequest, GuestDevice, GuestError, SevGuest};
    use crate::report::REPORT_LEN;
    use std::os::fd::AsRawFd as _;
    use zeroize::{Zeroize as _, Zeroizing};

    /// `struct snp_guest_request_ioctl`.
    #[repr(C)]
    struct GuestRequest {
        msg_version: u8,
        req_data: u64,
        resp_data: u64,
        exitinfo2: u64,
    }

    /// `struct snp_report_req`.
    #[repr(C)]
    struct ReportRequest {
        user_data: [u8; 64],
        vmpl: u32,
        reserved: [u8; 28],
    }

    /// `struct snp_derived_key_req`.
    #[repr(C)]
    struct KeyRequest {
        root_key_select: u32,
        reserved: u32,
        guest_field_select: u64,
        vmpl: u32,
        guest_svn: u32,
        tcb_version: u64,
    }

    /// `struct snp_report_resp` / `snp_derived_key_resp`: the firmware's
    /// response message.
    #[repr(C)]
    struct Response<const N: usize> {
        data: [u8; N],
    }

    // _IOWR('S', nr, struct snp_guest_request_ioctl): read|write, size 32.
    const SNP_GET_REPORT: u64 = 0xC020_5300;
    const SNP_GET_DERIVED_KEY: u64 = 0xC020_5301;
    /// Where a response message's payload starts (after status, size and
    /// reserved bytes).
    const PAYLOAD: usize = 0x20;

    impl SevGuest {
        fn request<Req, const N: usize>(
            &self,
            name: &'static str,
            number: u64,
            request: &mut Req,
            response: &mut Response<N>,
        ) -> Result<(), GuestError> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.path)
                .map_err(|e| GuestError::Open {
                    path: self.path.display().to_string(),
                    error: e.to_string(),
                })?;
            let mut ioctl = GuestRequest {
                msg_version: 1,
                req_data: std::ptr::from_mut(request) as u64,
                resp_data: std::ptr::from_mut(response) as u64,
                exitinfo2: 0,
            };
            // SAFETY: `ioctl` points at a live, correctly laid out
            // `snp_guest_request_ioctl` whose request and response pointers
            // point at live buffers of the sizes the kernel reads and writes
            // for this request number, and all of them outlive the call.
            #[allow(clippy::useless_conversion, clippy::cast_possible_truncation)]
            let status = unsafe {
                libc::ioctl(
                    file.as_raw_fd(),
                    number as _,
                    std::ptr::from_mut(&mut ioctl),
                )
            };
            if status < 0 {
                return Err(GuestError::Request {
                    request: name,
                    fw: (ioctl.exitinfo2 & 0xFFFF_FFFF) as u32,
                    vmm: (ioctl.exitinfo2 >> 32) as u32,
                    os: std::io::Error::last_os_error().to_string(),
                });
            }
            let status = u32::from_le_bytes(response.data[..4].try_into().unwrap());
            if status != 0 {
                return Err(GuestError::Status {
                    request: name,
                    status,
                });
            }
            Ok(())
        }
    }

    impl GuestDevice for SevGuest {
        fn report(&self, report_data: &[u8; 64]) -> Result<Vec<u8>, GuestError> {
            let mut request = ReportRequest {
                user_data: *report_data,
                vmpl: 0,
                reserved: [0; 28],
            };
            let mut response = Response { data: [0u8; 4000] };
            // A status the firmware must overwrite: a device that answers
            // without writing one isn't taken for success.
            response.data[..4].fill(0xFF);
            self.request("report", SNP_GET_REPORT, &mut request, &mut response)?;
            Ok(response.data[PAYLOAD..PAYLOAD + REPORT_LEN].to_vec())
        }

        fn derived_key(
            &self,
            request: &DerivedKeyRequest,
        ) -> Result<Zeroizing<[u8; 32]>, GuestError> {
            let mut key_request = KeyRequest {
                root_key_select: 0, // the chip's VCEK
                reserved: 0,
                guest_field_select: request.guest_field_select,
                vmpl: request.vmpl,
                guest_svn: request.guest_svn,
                tcb_version: request.tcb_version,
            };
            let mut response = Response { data: [0u8; 64] };
            response.data[..4].fill(0xFF);
            let result = self.request(
                "derived key",
                SNP_GET_DERIVED_KEY,
                &mut key_request,
                &mut response,
            );
            let key = result.and_then(|()| {
                let mut key = Zeroizing::new([0u8; 32]);
                key.copy_from_slice(&response.data[PAYLOAD..PAYLOAD + 32]);
                // No real derived key is all zeros: a device that didn't
                // write one is not to seal with.
                if key.iter().all(|b| *b == 0) {
                    return Err(GuestError::Status {
                        request: "derived key",
                        status: 0,
                    });
                }
                Ok(key)
            });
            response.data.zeroize();
            key
        }
    }
}

#[cfg(all(feature = "guest", not(target_os = "linux")))]
impl GuestDevice for SevGuest {
    fn report(&self, _report_data: &[u8; 64]) -> Result<Vec<u8>, GuestError> {
        Err(GuestError::Unsupported)
    }

    fn derived_key(&self, _request: &DerivedKeyRequest) -> Result<Zeroizing<[u8; 32]>, GuestError> {
        Err(GuestError::Unsupported)
    }
}

/// The launch identity a [`TestGuest`] reports and derives keys from.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestIdentity {
    pub policy: u64,
    pub family_id: [u8; 16],
    pub image_id: [u8; 16],
    pub measurement: [u8; 48],
    pub guest_svn: u32,
    pub id_key_digest: [u8; 48],
    pub chip_id: [u8; 64],
}

#[cfg(any(test, feature = "test-support"))]
impl Default for TestIdentity {
    fn default() -> Self {
        TestIdentity {
            // SMT allowed, ABI 0.31: a policy a real launch could use.
            policy: 0x3_0000 | (1 << 16),
            family_id: [0xFA; 16],
            image_id: [0x1A; 16],
            measurement: [0x4D; 48],
            guest_svn: 1,
            id_key_digest: [0x1D; 48],
            chip_id: [0xC1; 64],
        }
    }
}

/// A stand-in security processor for tests: "chip" secret, launch identity,
/// and a P-384 key standing in for the VCEK (see [`TestGuest::vcek`]).
#[cfg(any(test, feature = "test-support"))]
pub struct TestGuest {
    chip_secret: [u8; 32],
    pub identity: TestIdentity,
    vcek: p384::ecdsa::SigningKey,
}

#[cfg(any(test, feature = "test-support"))]
impl TestGuest {
    /// A guest on the "chip" named by `chip_secret` (the same secret is the
    /// same chip), with `identity`.
    pub fn new(chip_secret: [u8; 32], identity: TestIdentity) -> Self {
        use sha2::Digest as _;
        let seed: [u8; 48] = sha2::Sha384::digest(chip_secret).into();
        let vcek = p384::ecdsa::SigningKey::from_slice(&seed).expect("a valid P-384 scalar");
        TestGuest {
            chip_secret,
            identity,
            vcek,
        }
    }

    /// The key that signs this guest's reports, which a test trusts in place
    /// of AMD's chain.
    pub fn vcek(&self) -> p384::ecdsa::VerifyingKey {
        *self.vcek.verifying_key()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl GuestDevice for TestGuest {
    fn report(&self, report_data: &[u8; 64]) -> Result<Vec<u8>, GuestError> {
        use crate::report::{REPORT_LEN, SIGNED_LEN};
        let id = &self.identity;
        let mut raw = vec![0u8; REPORT_LEN];
        raw[0x00..0x04].copy_from_slice(&3u32.to_le_bytes());
        raw[0x04..0x08].copy_from_slice(&id.guest_svn.to_le_bytes());
        raw[0x08..0x10].copy_from_slice(&id.policy.to_le_bytes());
        raw[0x10..0x20].copy_from_slice(&id.family_id);
        raw[0x20..0x30].copy_from_slice(&id.image_id);
        raw[0x34..0x38].copy_from_slice(&1u32.to_le_bytes());
        raw[0x50..0x90].copy_from_slice(report_data);
        raw[0x90..0xC0].copy_from_slice(&id.measurement);
        raw[0xE0..0x110].copy_from_slice(&id.id_key_digest);
        raw[0x1A0..0x1E0].copy_from_slice(&id.chip_id);
        let signature: p384::ecdsa::Signature =
            ecdsa::signature::Signer::sign(&self.vcek, &raw[..SIGNED_LEN]);
        let (r, s) = (signature.r().to_bytes(), signature.s().to_bytes());
        for i in 0..48 {
            raw[SIGNED_LEN + i] = r[47 - i];
            raw[SIGNED_LEN + 72 + i] = s[47 - i];
        }
        Ok(raw)
    }

    fn derived_key(&self, request: &DerivedKeyRequest) -> Result<Zeroizing<[u8; 32]>, GuestError> {
        use sha2::Digest as _;
        let id = &self.identity;
        let select = request.guest_field_select;
        let mut hash = sha2::Sha256::new();
        hash.update(self.chip_secret);
        hash.update(select.to_le_bytes());
        hash.update(request.vmpl.to_le_bytes());
        let mix = |hash: &mut sha2::Sha256, bit: u64, bytes: &[u8]| {
            if select & bit != 0 {
                hash.update(bytes);
            }
        };
        mix(&mut hash, FIELD_POLICY, &id.policy.to_le_bytes());
        mix(&mut hash, FIELD_IMAGE_ID, &id.image_id);
        mix(&mut hash, FIELD_FAMILY_ID, &id.family_id);
        mix(&mut hash, FIELD_MEASUREMENT, &id.measurement);
        mix(&mut hash, FIELD_GUEST_SVN, &request.guest_svn.to_le_bytes());
        mix(
            &mut hash,
            FIELD_TCB_VERSION,
            &request.tcb_version.to_le_bytes(),
        );
        Ok(Zeroizing::new(hash.finalize().into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{parse, Product};

    fn request(select: u64) -> DerivedKeyRequest {
        DerivedKeyRequest {
            guest_field_select: select,
            vmpl: 0,
            guest_svn: 0,
            tcb_version: 0,
        }
    }

    /// The stand-in behaves like the firmware where key custody depends on
    /// it: a selected field that differs gives a different key, an
    /// unselected one doesn't, and another chip never gets the same key.
    #[test]
    fn a_derived_key_changes_with_what_it_is_bound_to() {
        let select = FIELD_POLICY | FIELD_MEASUREMENT;
        let guest = TestGuest::new([1; 32], TestIdentity::default());
        let key = guest.derived_key(&request(select)).unwrap();
        assert_eq!(*key, *guest.derived_key(&request(select)).unwrap());

        let mut other_image = TestIdentity::default();
        other_image.measurement[0] ^= 1;
        let upgraded = TestGuest::new([1; 32], other_image);
        assert_ne!(*key, *upgraded.derived_key(&request(select)).unwrap());

        let same_measurement = TestIdentity {
            image_id: [9; 16],
            ..TestIdentity::default()
        };
        let relabelled = TestGuest::new([1; 32], same_measurement);
        assert_eq!(
            *key,
            *relabelled.derived_key(&request(select)).unwrap(),
            "the image ID is not selected"
        );

        let other_chip = TestGuest::new([2; 32], TestIdentity::default());
        assert_ne!(*key, *other_chip.derived_key(&request(select)).unwrap());
    }

    /// A stand-in report parses, carries the identity and report data, and
    /// its signature verifies against the stand-in VCEK.
    #[test]
    fn a_test_report_is_signed_and_carries_its_identity() {
        let guest = TestGuest::new([1; 32], TestIdentity::default());
        let raw = guest.report(&[0x5A; 64]).unwrap();
        let report = parse(&raw, Product::Genoa).unwrap();
        assert_eq!(report.report_data, [0x5A; 64]);
        assert_eq!(report.measurement, guest.identity.measurement);
        assert_eq!(report.id_key_digest, guest.identity.id_key_digest);
        assert_eq!(report.guest_svn, 1);
        assert!(!report.debug_allowed());
        crate::verify::verify_report_signed_by(&report, &guest.vcek()).unwrap();
    }

    /// Without the device there is a clear error, not a panic.
    #[cfg(all(feature = "guest", target_os = "linux"))]
    #[test]
    fn a_missing_device_is_reported_by_path() {
        let guest = SevGuest::new("/nonexistent/sev-guest");
        let error = guest.report(&[0; 64]).unwrap_err().to_string();
        assert!(error.contains("/nonexistent/sev-guest"), "{error}");
    }

    /// The real device, when this runs inside an SEV-SNP guest: a report
    /// with our data in it, and a stable derived key. Skipped elsewhere.
    #[cfg(all(feature = "guest", target_os = "linux"))]
    #[test]
    fn the_real_device_answers_inside_a_guest() {
        let path = "/dev/sev-guest";
        if !std::path::Path::new(path).exists() {
            eprintln!("skipped: no {path} (not an SEV-SNP guest)");
            return;
        }
        let guest = SevGuest::new(path);
        let raw = guest.report(&[0x42; 64]).unwrap();
        let report = parse(&raw, Product::Genoa).unwrap();
        assert_eq!(report.report_data, [0x42; 64]);
        let select = FIELD_POLICY | FIELD_MEASUREMENT;
        assert_eq!(
            *guest.derived_key(&request(select)).unwrap(),
            *guest.derived_key(&request(select)).unwrap()
        );
    }
}
