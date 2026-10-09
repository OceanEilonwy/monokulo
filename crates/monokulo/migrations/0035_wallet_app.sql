-- Which wallet app a brought-in wallet is in, as its owner said when adding
-- it ("Which app is it in?"): one of `crate::wallets::WALLET_APPS`' keys
-- ('cake', 'monerocom', 'stack', 'feather', 'gui') or 'other'. NULL when
-- not said, and for a wallet made here, whose `backup` says where it is.
ALTER TABLE wallets ADD COLUMN app TEXT;
