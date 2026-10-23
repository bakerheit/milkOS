//! ECDSA signatures as DER (SEC 1 C.5, RFC 5480): `Ecdsa-Sig-Value ::= SEQUENCE { r
//! INTEGER, s INTEGER }`, parsed strictly by [`parse_der`] for [`crate::p256::verify_der`]
//! and [`crate::p384::verify_der`]. Strict means one encoding per signature (DER, not BER):
//! definite lengths in their shortest form, integers positive and minimal, nothing after.

/// Read a DER length at `b[0..]`: (length, bytes used). Short form below 128, else 0x81
/// with a byte >= 128 or 0x82 with a value >= 256 (longer forms are refused: nothing here is
/// that big). Indefinite (0x80) is refused.
fn der_len(b: &[u8]) -> Option<(usize, usize)> {
    let first = *b.first()?;
    match first {
        0..=0x7f => Some((first as usize, 1)),
        0x81 => {
            let v = *b.get(1)? as usize;
            (v >= 0x80).then_some((v, 2))
        }
        0x82 => {
            let v = ((*b.get(1)? as usize) << 8) | *b.get(2)? as usize;
            (v >= 0x100).then_some((v, 3))
        }
        _ => None,
    }
}

/// One INTEGER at the start of `b`: (its magnitude without a sign-padding zero, the rest).
fn der_uint(b: &[u8]) -> Option<(&[u8], &[u8])> {
    if *b.first()? != 0x02 {
        return None;
    }
    let (len, used) = der_len(b.get(1..)?)?;
    let start = 1 + used;
    let end = start.checked_add(len)?;
    let v = b.get(start..end)?;
    let rest = b.get(end..)?;
    let first = *v.first()?; // a zero-length INTEGER is invalid
    if first & 0x80 != 0 {
        return None; // negative
    }
    if first == 0 && v.len() > 1 {
        // A leading zero is only allowed (and required) before a byte with the top bit set.
        if v[1] & 0x80 == 0 {
            return None;
        }
        return Some((&v[1..], rest));
    }
    Some((v, rest))
}

/// Strict DER `SEQUENCE { r INTEGER, s INTEGER }` with nothing after it. Returns (r, s) as
/// big-endian magnitudes (the sign-padding zero dropped; a zero integer comes back as
/// `[0]`, which verification then refuses).
pub fn parse_der(sig: &[u8]) -> Option<(&[u8], &[u8])> {
    if *sig.first()? != 0x30 {
        return None;
    }
    let (len, used) = der_len(sig.get(1..)?)?;
    let start = 1 + used;
    if start.checked_add(len)? != sig.len() {
        return None;
    }
    let body = sig.get(start..)?;
    let (r, rest) = der_uint(body)?;
    let (s, rest) = der_uint(rest)?;
    rest.is_empty().then_some((r, s))
}

#[cfg(test)]
mod tests {
    use super::parse_der;
    use crate::unhex_vec as h;

    #[test]
    fn strict_der() {
        // Good: small values, and a value needing the sign-padding zero.
        assert_eq!(
            parse_der(&h("3006020101020102")),
            Some((&[1u8][..], &[2u8][..]))
        );
        assert_eq!(
            parse_der(&h("3008020300ff01020101")),
            Some((&[0xffu8, 0x01][..], &[1u8][..]))
        );
        assert_eq!(
            parse_der(&h("3006020100020101")),
            Some((&[0u8][..], &[1u8][..]))
        );
        let bad = [
            "",
            "30",
            "3000",
            "3106020101020102",       // not a SEQUENCE
            "3007020101020102",       // length too long
            "3005020101020102",       // length too short
            "300602010102010200",     // trailing byte
            "308106020101020102",     // long form for a short length
            "3080020101020102",       // indefinite length
            "30070202000102 0102",    // needless leading zero
            "3006020181020102",       // negative r
            "3006020101020182",       // negative s
            "30050200020101",         // zero-length integer
            "3006030101020102",       // BIT STRING, not INTEGER
            "300602810101020102",     // long-form integer length
            "3003020101",             // only r
            "3009020101020102020103", // three integers
        ];
        for b in bad {
            let v = h(&b.replace(' ', ""));
            assert!(parse_der(&v).is_none(), "accepted {b}");
        }
        // Long form where it is needed: a 0x81 length >= 128.
        let mut big = h("308184");
        big.extend(h("0240"));
        big.extend([0x11; 64]);
        big.extend(h("0240"));
        big.extend([0x22; 64]);
        assert_eq!(big.len(), 3 + 132);
        assert!(parse_der(&big).is_some());
    }

    #[test]
    fn der_never_panics() {
        // Every prefix and every single-byte change of a valid signature.
        let good = h(
            "3046022100d7c7a3d1b3a1e2c4f5a6b7c8d9e0f1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8022100e8d7c6b5a4f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d7",
        );
        assert!(parse_der(&good).is_some());
        for n in 0..good.len() {
            let _ = parse_der(&good[..n]);
        }
        for i in 0..good.len() {
            for v in [0u8, 1, 0x7f, 0x80, 0x81, 0x82, 0xff] {
                let mut m = good.clone();
                m[i] = v;
                let _ = parse_der(&m);
            }
        }
    }
}
