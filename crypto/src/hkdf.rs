//! HKDF (RFC 5869) over any [`Hash`]: [`extract`] and [`expand`], the key derivation under
//! the TLS 1.3 key schedule. [`selftest`]: RFC 5869 cases 1 to 3 (SHA-256), and SHA-384
//! vectors cross-checked with Python's `hmac` module.

use crate::hash::{Hash, MAX_OUT};
use crate::hmac::Hmac;

/// PRK = HMAC-Hash(salt, IKM) into the front of `prk` (H::OUT bytes). An empty salt means
/// H::OUT zero bytes (which HMAC pads to the same key).
pub fn extract<H: Hash>(salt: &[u8], ikm: &[u8], prk: &mut [u8]) {
    let mut h = Hmac::<H>::new(salt);
    h.update(ikm);
    h.finish_into(prk);
}

/// OKM = T(1) | T(2) | ... into `okm`. False (and `okm` untouched) if more than 255 * H::OUT
/// bytes are asked for, which RFC 5869 forbids.
pub fn expand<H: Hash>(prk: &[u8], info: &[u8], okm: &mut [u8]) -> bool {
    if okm.len() > 255 * H::OUT {
        return false;
    }
    let base = Hmac::<H>::new(prk);
    let mut t = [0u8; MAX_OUT];
    let mut tlen = 0;
    for (i, chunk) in okm.chunks_mut(H::OUT).enumerate() {
        let mut h = base.clone();
        h.update(&t[..tlen]).update(info).update(&[(i + 1) as u8]);
        h.finish_into(&mut t);
        tlen = H::OUT;
        chunk.copy_from_slice(&t[..chunk.len()]);
    }
    crate::wipe(&mut t);
    true
}

pub fn selftest() -> (usize, usize) {
    use crate::sha256::Sha256;
    use crate::sha512::Sha384;
    use crate::unhex_vec as h;
    let mut ok = 0;
    let mut total = 0;
    let mut check = |ikm: &[u8], salt: &[u8], info: &[u8], prk_want: &str, okm_want: &str, is384: bool| {
        let mut prk = [0u8; 64];
        let mut okm = alloc::vec![0u8; okm_want.len() / 2];
        let n = if is384 {
            extract::<Sha384>(salt, ikm, &mut prk);
            expand::<Sha384>(&prk[..48], info, &mut okm);
            48
        } else {
            extract::<Sha256>(salt, ikm, &mut prk);
            expand::<Sha256>(&prk[..32], info, &mut okm);
            32
        };
        total += 2;
        ok += (crate::hex(&prk[..n]) == prk_want) as usize;
        ok += (crate::hex(&okm) == okm_want) as usize;
    };
    let ikm2: alloc::vec::Vec<u8> = (0x00..=0x4f).collect();
    let salt2: alloc::vec::Vec<u8> = (0x60..=0xaf).collect();
    let info2: alloc::vec::Vec<u8> = (0xb0..=0xff).collect();
    // RFC 5869 A.1, A.2, A.3.
    check(
        &[0x0b; 22],
        &h("000102030405060708090a0b0c"),
        &h("f0f1f2f3f4f5f6f7f8f9"),
        "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5",
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865",
        false,
    );
    check(
        &ikm2,
        &salt2,
        &info2,
        "06a6b88c5853361a06104c9ceb35b45cef760014904671014a193f40c15fc244",
        "b11e398dc80327a1c8e7f78c596a49344f012eda2d4efad8a050cc4c19afa97c59045a99cac7827271cb41c65e590e09da3275600c2f09b8367793a9aca3db71cc30c58179ec3e87c14c01d5c1f3434f1d87",
        false,
    );
    check(
        &[0x0b; 22],
        &[],
        &[],
        "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04",
        "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8",
        false,
    );
    // SHA-384 (inputs of A.1 and A.3), from Python's hmac module.
    check(
        &[0x0b; 22],
        &h("000102030405060708090a0b0c"),
        &h("f0f1f2f3f4f5f6f7f8f9"),
        SHA384_A1_PRK,
        SHA384_A1_OKM,
        true,
    );
    check(&[0x0b; 22], &[], &[], SHA384_A3_PRK, SHA384_A3_OKM, true);
    // Too long is refused.
    let mut big = alloc::vec![0u8; 255 * 32 + 1];
    total += 1;
    ok += !expand::<Sha256>(&[1; 32], b"", &mut big) as usize;
    (ok, total)
}

const SHA384_A1_PRK: &str = "704b39990779ce1dc548052c7dc39f303570dd13fb39f7acc564680bef80e8dec70ee9a7e1f3e293ef68eceb072a5ade";
const SHA384_A1_OKM: &str = "9b5097a86038b805309076a44b3a9f38063e25b516dcbf369f394cfab43685f748b6457763e4f0204fc5";
const SHA384_A3_PRK: &str = "10e40cf072a4c5626e43dd22c1cf727d4bb140975c9ad0cbc8e45b40068f8f0ba57cdb598af9dfa6963a96899af047e5";
const SHA384_A3_OKM: &str = "c8c96e710f89b0d7990bca68bcdec8cf854062e54c73a7abc743fade9b242daacc1cea5670415b52849c";
