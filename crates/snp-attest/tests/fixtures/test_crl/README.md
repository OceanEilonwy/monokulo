# Test CRL material

Not AMD's. A throwaway RSA-PSS (SHA-384) test CA, two leaf certificates it
issued (`revoked.der`, serial 0x1000, and `good.der`, serial 0x1001), and a
CRL from that CA listing `revoked.der` (`crl.der`), all DER. The keys were
discarded. Made with `openssl ca` so the revocation check can be tested
against a CRL that lists something: AMD's real CRLs (`../*_crl.der`,
fetched from `https://kdsintf.amd.com/vcek/v1/{product}/crl` on
2026-10-01, signed by each product's ARK) list nothing yet.
