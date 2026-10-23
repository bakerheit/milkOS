//! RFC 8448 section 3, "Simple 1-RTT Handshake": the key schedule and record protection
//! byte for byte, then the whole trace replayed through the real client (its ClientHello
//! and X25519 key swapped in; the trace's certificate has no subjectAltName and a 1024-bit
//! key, so the chain checks are off for this test only, and the CertificateVerify is still
//! verified).
//!
//! Every value below is self-checking: the certificate's own signature, the server's
//! CertificateVerify (RSA-PSS over the transcript) and both Finished MACs only verify if the
//! bytes are exactly the RFC's, and the AEAD tags of the protected records only open with
//! exactly the RFC's keys.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::keys::{self, CipherSuite, RecordKeys, Schedule, Secret, Transcript};
use crate::x509::{Certificate, Hash, SigAlg};
use crate::{Client, Config, IoError, Policy, RootStore, TestHello, Transport};

fn h(s: &str) -> Vec<u8> {
    let s: Vec<u8> = s.bytes().filter(|c| c.is_ascii_hexdigit()).collect();
    assert!(s.len() % 2 == 0);
    s.chunks(2)
        .map(|p| u8::from_str_radix(core::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

fn h32(s: &str) -> [u8; 32] {
    h(s).try_into().unwrap()
}

const CLIENT_X25519_PRIVATE: &str = "49 af 42 ba 7f 79 94 85 2d 71 3e f2 78 4b cb ca a7 91 1d e2 6a dc 56 42 cb 63 45 40 e7 ea 50 05";
const CLIENT_X25519_PUBLIC: &str = "99 38 1d e5 60 e4 bd 43 d2 3d 8e 43 5a 7d ba fe b3 c0 6e 51 c1 3c ae 4d 54 13 69 1e 52 9a af 2c";
const SERVER_X25519_PRIVATE: &str = "b1 58 0e ea df 6d d5 89 b8 ef 4f 2d 56 52 57 8c c8 10 e9 98 01 91 ec 8d 05 83 08 ce a2 16 a2 1e";
const SERVER_X25519_PUBLIC: &str = "c9 82 88 76 11 20 95 fe 66 76 2b db f7 c6 72 e1 56 d6 cc 25 3b 83 3d f1 dd 69 b1 b0 4e 75 1f 0f";

const CLIENT_HELLO: &str = "01 00 00 c0 03 03 cb 34 ec b1 e7 81 63
ba 1c 38 c6 da cb 19 6a 6d ff a2 1a 8d 99 12 ec 18 a2 ef 62 83
02 4d ec e7 00 00 06 13 01 13 03 13 02 01 00 00 91 00 00 00 0b
00 09 00 00 06 73 65 72 76 65 72 ff 01 00 01 00 00 0a 00 14 00
12 00 1d 00 17 00 18 00 19 01 00 01 01 01 02 01 03 01 04 00 23
00 00 00 33 00 26 00 24 00 1d 00 20 99 38 1d e5 60 e4 bd 43 d2
3d 8e 43 5a 7d ba fe b3 c0 6e 51 c1 3c ae 4d 54 13 69 1e 52 9a
af 2c 00 2b 00 03 02 03 04 00 0d 00 20 00 1e 04 03 05 03 06 03
02 03 08 04 08 05 08 06 04 01 05 01 06 01 02 01 04 02 05 02 06
02 02 02 00 2d 00 02 01 01 00 1c 00 02 40 01";

const SERVER_HELLO: &str = "02 00 00 56 03 03 a6 af 06 a4 12 18 60
dc 5e 6e 60 24 9c d3 4c 95 93 0c 8a c5 cb 14 34 da c1 55 77 2e
d3 e2 69 28 00 13 01 00 00 2e 00 33 00 24 00 1d 00 20 c9 82 88
76 11 20 95 fe 66 76 2b db f7 c6 72 e1 56 d6 cc 25 3b 83 3d f1
dd 69 b1 b0 4e 75 1f 0f 00 2b 00 02 03 04";

const ENCRYPTED_EXTENSIONS: &str = "08 00 00 24 00 22 00 0a 00 14 00
12 00 1d 00 17 00 18 00 19 01 00 01 01 01 02 01 03 01 04 00 1c
00 02 40 01 00 00 00 00";

const CERTIFICATE: &str = "0b 00 01 b9 00 00 01 b5 00 01 b0 30 82
01 ac 30 82 01 15 a0 03 02 01 02 02 01 02 30 0d 06 09 2a 86 48
86 f7 0d 01 01 0b 05 00 30 0e 31 0c 30 0a 06 03 55 04 03 13 03
72 73 61 30 1e 17 0d 31 36 30 37 33 30 30 31 32 33 35 39 5a 17
0d 32 36 30 37 33 30 30 31 32 33 35 39 5a 30 0e 31 0c 30 0a 06
03 55 04 03 13 03 72 73 61 30 81 9f 30 0d 06 09 2a 86 48 86 f7
0d 01 01 01 05 00 03 81 8d 00 30 81 89 02 81 81 00 b4 bb 49 8f
82 79 30 3d 98 08 36 39 9b 36 c6 98 8c 0c 68 de 55 e1 bd b8 26
d3 90 1a 24 61 ea fd 2d e4 9a 91 d0 15 ab bc 9a 95 13 7a ce 6c
1a f1 9e aa 6a f9 8c 7c ed 43 12 09 98 e1 87 a8 0e e0 cc b0 52
4b 1b 01 8c 3e 0b 63 26 4d 44 9a 6d 38 e2 2a 5f da 43 08 46 74
80 30 53 0e f0 46 1c 8c a9 d9 ef bf ae 8e a6 d1 d0 3e 2b d1 93
ef f0 ab 9a 80 02 c4 74 28 a6 d3 5a 8d 88 d7 9f 7f 1e 3f 02 03
01 00 01 a3 1a 30 18 30 09 06 03 55 1d 13 04 02 30 00 30 0b 06
03 55 1d 0f 04 04 03 02 05 a0 30 0d 06 09 2a 86 48 86 f7 0d 01
01 0b 05 00 03 81 81 00 85 aa d2 a0 e5 b9 27 6b 90 8c 65 f7 3a
72 67 17 06 18 a5 4c 5f 8a 7b 33 7d 2d f7 a5 94 36 54 17 f2 ea
e8 f8 a5 8c 8f 81 72 f9 31 9c f3 6b 7f d6 c5 5b 80 f2 1a 03 01
51 56 72 60 96 fd 33 5e 5e 67 f2 db f1 02 70 2e 60 8c ca e6 be
c1 fc 63 a4 2a 99 be 5c 3e b7 10 7c 3c 54 e9 b9 eb 2b d5 20 3b
1c 3b 84 e0 a8 b2 f7 59 40 9b a3 ea c9 d9 1d 40 2d cc 0c c8 f8
96 12 29 ac 91 87 b4 2b 4d e1 00 00";

const CERTIFICATE_VERIFY: &str = "0f 00 00 84 08 04 00 80 5a 74 7c
5d 88 fa 9b d2 e5 5a b0 85 a6 10 15 b7 21 1f 82 4c d4 84 14 5a
b3 ff 52 f1 fd a8 47 7b 0b 7a bc 90 db 78 e2 d3 3a 5c 14 1a 07
86 53 fa 6b ef 78 0c 5e a2 48 ee aa a7 85 c4 f3 94 ca b6 d3 0b
be 8d 48 59 ee 51 1f 60 29 57 b1 54 11 ac 02 76 71 45 9e 46 44
5c 9e a5 8c 18 1e 81 8e 95 b8 c3 fb 0b f3 27 84 09 d3 be 15 2a
3d a5 04 3e 06 3d da 65 cd f5 ae a2 0d 53 df ac d4 2f 74 f3";

const SERVER_FINISHED: &str = "14 00 00 20 9b 9b 14 1d 90 63 37 fb d2
cb dc e7 1d f4 de da 4a b4 2c 30 95 72 cb 7f ff ee 54 54 b7 8f
07 18";

const CLIENT_FINISHED: &str = "14 00 00 20 a8 ec 43 6d 67 76 34 ae 52
5a c1 fc eb e1 1a 03 9e c1 76 94 fa c6 e9 85 27 b6 42 f2 ed d5
ce 61";

/// The server's protected handshake record (EncryptedExtensions..Finished).
const SERVER_FLIGHT_RECORD: &str = "17 03 03 02 a2 d1 ff 33 4a 56 f5 bf
f6 59 4a 07 cc 87 b5 80 23 3f 50 0f 45 e4 89 e7 f3 3a f3 5e df
78 69 fc f4 0a a4 0a a2 b8 ea 73 f8 48 a7 ca 07 61 2e f9 f9 45
cb 96 0b 40 68 90 51 23 ea 78 b1 11 b4 29 ba 91 91 cd 05 d2 a3
89 28 0f 52 61 34 aa dc 7f c7 8c 4b 72 9d f8 28 b5 ec f7 b1 3b
d9 ae fb 0e 57 f2 71 58 5b 8e a9 bb 35 5c 7c 79 02 07 16 cf b9
b1 18 3e f3 ab 20 e3 7d 57 a6 b9 d7 47 76 09 ae e6 e1 22 a4 cf
51 42 73 25 25 0c 7d 0e 50 92 89 44 4c 9b 3a 64 8f 1d 71 03 5d
2e d6 5b 0e 3c dd 0c ba e8 bf 2d 0b 22 78 12 cb b3 60 98 72 55
cc 74 41 10 c4 53 ba a4 fc d6 10 92 8d 80 98 10 e4 b7 ed 1a 8f
d9 91 f0 6a a6 24 82 04 79 7e 36 a6 a7 3b 70 a2 55 9c 09 ea d6
86 94 5b a2 46 ab 66 e5 ed d8 04 4b 4c 6d e3 fc f2 a8 94 41 ac
66 27 2f d8 fb 33 0e f8 19 05 79 b3 68 45 96 c9 60 bd 59 6e ea
52 0a 56 a8 d6 50 f5 63 aa d2 74 09 96 0d ca 63 d3 e6 88 61 1e
a5 e2 2f 44 15 cf 95 38 d5 1a 20 0c 27 03 42 72 96 8a 26 4e d6
54 0c 84 83 8d 89 f7 2c 24 46 1a ad 6d 26 f5 9e ca ba 9a cb bb
31 7b 66 d9 02 f4 f2 92 a3 6a c1 b6 39 c6 37 ce 34 31 17 b6 59
62 22 45 31 7b 49 ee da 0c 62 58 f1 00 d7 d9 61 ff b1 38 64 7e
92 ea 33 0f ae ea 6d fa 31 c7 a8 4d c3 bd 7e 1b 7a 6c 71 78 af
36 87 90 18 e3 f2 52 10 7f 24 3d 24 3d c7 33 9d 56 84 c8 b0 37
8b f3 02 44 da 8c 87 c8 43 f5 e5 6e b4 c5 e8 28 0a 2b 48 05 2c
f9 3b 16 49 9a 66 db 7c ca 71 e4 59 94 26 f7 d4 61 e6 6f 99 88
2b d8 9f c5 08 00 be cc a6 2d 6c 74 11 6d bd 29 72 fd a1 fa 80
f8 5d f8 81 ed be 5a 37 66 89 36 b3 35 58 3b 59 91 86 dc 5c 69
18 a3 96 fa 48 a1 81 d6 b6 fa 4f 9d 62 d5 13 af bb 99 2f 2b 99
2f 67 f8 af e6 7f 76 91 3f a3 88 cb 56 30 c8 ca 01 e0 c6 5d 11
c6 6a 1e 2a c4 c8 59 77 b7 c7 a6 99 9b bf 10 dc 35 ae 69 f5 51
56 14 63 6c 0b 9b 68 c1 9e d2 e3 1c 0b 3b 66 76 30 38 eb ba 42
f3 b3 8e dc 03 99 f3 a9 f2 3f aa 63 97 8c 31 7f c9 fa 66 a7 3f
60 f0 50 4d e9 3b 5b 84 5e 27 55 92 c1 23 35 ee 34 0b bc 4f dd
d5 02 78 40 16 e4 b3 be 7e f0 4d da 49 f4 b4 40 a3 0c b5 d2 af
93 98 28 fd 4a e3 79 4e 44 f9 4d f5 a6 31 ed e4 2c 17 19 bf da
bf 02 53 fe 51 75 be 89 8e 75 0e dc 53 37 0d 2b";

/// The client's protected Finished record.
const CLIENT_FINISHED_RECORD: &str = "17 03 03 00 35 75 ec 4d c2 38 cc
e6 0b 29 80 44 a7 1e 21 9c 56 cc 77 b0 51 7f e9 b9 3c 7a 4b fc
44 d8 7f 38 f8 03 38 ac 98 fc 46 de b3 84 bd 1c ae ac ab 68 67
d7 26 c4 05 46";

/// The server's NewSessionTicket record.
const TICKET_RECORD: &str = "17 03 03 00 de 3a 6b 8f 90 41 4a 97
d6 95 9c 34 87 68 0d e5 13 4a 2b 24 0e 6c ff ac 11 6e 95 d4 1d
6a f8 f6 b5 80 dc f3 d1 1d 63 c7 58 db 28 9a 01 59 40 25 2f 55
71 3e 06 1d c1 3e 07 88 91 a3 8e fb cf 57 53 ad 8e f1 70 ad 3c
73 53 d1 6d 9d a7 73 b9 ca 7f 2b 9f a1 b6 c0 d4 a3 d0 3f 75 e0
9c 30 ba 1e 62 97 2a c4 6f 75 f7 b9 81 be 63 43 9b 29 99 ce 13
06 46 15 13 98 91 d5 e4 c5 b4 06 f1 6e 3f c1 81 a7 7c a4 75 84
00 25 db 2f 0a 77 f8 1b 5a b0 5b 94 c0 13 46 75 5f 69 23 2c 86
51 9d 86 cb ee ac 87 aa c3 47 d1 43 f9 60 5d 64 f6 50 db 4d 02
3e 70 e9 52 ca 49 fe 51 37 12 1c 74 bc 26 97 68 7e 24 87 46 d6
df 35 30 05 f3 bc e1 86 96 12 9c 81 53 55 6b 3b 6c 67 79 b3 7b
f1 59 85 68 4f";

/// 50 bytes 00..31 from the client, and the same from the server.
const CLIENT_DATA_RECORD: &str = "17 03 03 00 43 a2 3f 70 54 b6 2c 94
d0 af fa fe 82 28 ba 55 cb ef ac ea 42 f9 14 aa 66 bc ab 3f 2b
98 19 a8 a5 b4 6b 39 5b d5 4a 9a 20 44 1e 2b 62 97 4e 1f 5a 62
92 a2 97 70 14 bd 1e 3d ea e6 3a ee bb 21 69 49 15 e4";
const SERVER_DATA_RECORD: &str = "17 03 03 00 43 2e 93 7e 11 ef 4a c7
40 e5 38 ad 36 00 5f c4 a4 69 32 fc 32 25 d0 5f 82 aa 1b 36 e3
0e fa f9 7d 90 e6 df fc 60 2d cb 50 1a 59 a8 fc c4 9c 4b f2 e5
f0 a2 1c 00 47 c2 ab f3 32 54 0d d0 32 e1 67 c2 95 5d";
/// close_notify from the client, then from the server.
const CLIENT_ALERT_RECORD: &str = "17 03 03 00 13 c9 87 27 60 65 56 66
b7 4d 7f f1 15 3e fd 6d b6 d0 b0 e3";
const SERVER_ALERT_RECORD: &str = "17 03 03 00 13 b5 8f d6 71 66 eb f5
99 d2 47 20 cf be 7e fa 7a 88 64 a9";

#[test]
fn x25519_keys_and_shared_secret() {
    use milkcrypto::x25519;
    assert_eq!(x25519::public_key(&h32(CLIENT_X25519_PRIVATE)), h32(CLIENT_X25519_PUBLIC));
    assert_eq!(x25519::public_key(&h32(SERVER_X25519_PRIVATE)), h32(SERVER_X25519_PUBLIC));
    let shared = x25519::shared(&h32(CLIENT_X25519_PRIVATE), &h32(SERVER_X25519_PUBLIC)).unwrap();
    assert_eq!(
        shared,
        h32("8b d4 05 4f b5 5b 9d 63 fd fb ac f9 f0 4b 9f 0d 35 e6 d6 3f 53 75 63 ef d4 62 72 90 0f 89 49 2d")
    );
}

#[test]
fn certificate_is_the_rfcs() {
    // Self-signed: the signature checks only if every byte is right.
    let msg = h(CERTIFICATE);
    let der = &msg[4 + 1 + 3 + 3..msg.len() - 2];
    let c = Certificate::parse(der).unwrap();
    assert_eq!(c.sig_alg, Some(SigAlg::RsaPkcs1(Hash::Sha256)));
    let weak = Policy {
        min_rsa_bits: 1024,
        max_rsa_bits: 4096,
    };
    crate::verify::verify_signature(c.sig_alg.unwrap(), &c.key, c.tbs, c.signature, &weak).unwrap();
    // And the default policy refuses the 1024-bit key.
    assert_eq!(
        crate::verify::verify_signature(c.sig_alg.unwrap(), &c.key, c.tbs, c.signature, &Policy::default()),
        Err(crate::CertError::WeakKey)
    );
}

#[test]
fn key_schedule() {
    let early = keys::extract(&[0; 32], &[0; 32]);
    assert_eq!(
        early.0,
        h32("33 ad 0a 1c 60 7e c0 3b 09 e6 cd 98 93 68 0c e2 10 ad f3 00 aa 1f 26 60 e1 b2 2e 10 f1 70 f9 2a")
    );
    let derived = keys::derive_secret(&early, b"derived", &keys::empty_hash());
    assert_eq!(
        derived.0,
        h32("6f 26 15 a1 08 c7 02 c5 67 8f 54 fc 9d ba b6 97 16 c0 76 18 9c 48 25 0c eb ea c3 57 6c 36 11 ba")
    );
    let shared = h("8b d4 05 4f b5 5b 9d 63 fd fb ac f9 f0 4b 9f 0d 35 e6 d6 3f 53 75 63 ef d4 62 72 90 0f 89 49 2d");
    let mut t = Transcript::new();
    t.add(&h(CLIENT_HELLO));
    t.add(&h(SERVER_HELLO));
    let hello_hash = t.hash();
    assert_eq!(
        hello_hash,
        h32("86 0c 06 ed c0 78 58 ee 8e 78 f0 e7 42 8c 58 ed d6 b4 3f 2c a3 e6 e9 5f 02 ed 06 3c f0 e1 ca d8")
    );
    let s = Schedule::handshake(CipherSuite::Aes128GcmSha256, &shared, &hello_hash);
    assert_eq!(
        s.handshake_secret.0,
        h32("1d c8 26 e9 36 06 aa 6f dc 0a ad c1 2f 74 1b 01 04 6a a6 b9 9f 69 1e d2 21 a9 f0 ca 04 3f be ac")
    );
    assert_eq!(
        s.client_hs.0,
        h32("b3 ed db 12 6e 06 7f 35 a7 80 b3 ab f4 5e 2d 8f 3b 1a 95 07 38 f5 2e 96 00 74 6a 0e 27 a5 5a 21")
    );
    assert_eq!(
        s.server_hs.0,
        h32("b6 7b 7d 69 0c c1 6c 4e 75 e5 42 13 cb 2d 37 b4 e9 c9 12 bc de d9 10 5d 42 be fd 59 d3 91 ad 38")
    );
    let derived2 = keys::derive_secret(&s.handshake_secret, b"derived", &keys::empty_hash());
    assert_eq!(
        derived2.0,
        h32("43 de 77 e0 c7 77 13 85 9a 94 4d b9 db 25 90 b5 31 90 a6 5b 3e e2 e4 f1 2d d7 a0 bb 7c e2 54 b4")
    );
    let master = keys::extract(&derived2.0, &[0; 32]);
    assert_eq!(
        master.0,
        h32("18 df 06 84 3d 13 a0 8b f2 a4 49 84 4c 5f 8a 47 80 01 bc 4d 4c 62 79 84 d5 a4 1d a8 d0 40 29 19")
    );
    // Handshake traffic keys (key, iv) both ways.
    let kv = |secret: &Secret| {
        let mut k = [0u8; 16];
        let mut iv = [0u8; 12];
        keys::expand_label(&secret.0, b"key", b"", &mut k);
        keys::expand_label(&secret.0, b"iv", b"", &mut iv);
        (k.to_vec(), iv.to_vec())
    };
    assert_eq!(
        kv(&s.server_hs),
        (h("3f ce 51 60 09 c2 17 27 d0 f2 e4 e8 6e e4 03 bc"), h("5d 31 3e b2 67 12 76 ee 13 00 0b 30"))
    );
    assert_eq!(
        kv(&s.client_hs),
        (h("db fa a6 93 d1 76 2c 5b 66 6a f5 d9 50 25 8d 01"), h("5b d3 c7 1b 83 6e 0b 76 bb 73 26 5f"))
    );
    // Server Finished over CH..CertificateVerify.
    t.add(&h(ENCRYPTED_EXTENSIONS));
    t.add(&h(CERTIFICATE));
    t.add(&h(CERTIFICATE_VERIFY));
    assert_eq!(
        t.hash(),
        h32("ed b7 72 5f a7 a3 47 3b 03 1e c8 ef 65 a2 48 54 93 90 01 38 a2 b9 12 91 40 7d 79 51 a0 61 10 ed")
    );
    assert_eq!(keys::finished_verify_data(&s.server_hs, &t.hash()).to_vec(), h(SERVER_FINISHED)[4..]);
    t.add(&h(SERVER_FINISHED));
    // Client Finished over CH..server Finished.
    assert_eq!(keys::finished_verify_data(&s.client_hs, &t.hash()).to_vec(), h(CLIENT_FINISHED)[4..]);
    // Application traffic keys.
    let (c_ap, s_ap) = s.application(&t.hash());
    assert_eq!(
        kv(&s_ap),
        (h("9f 02 28 3b 6c 9c 07 ef c2 6b b9 f2 ac 92 e3 56"), h("cf 78 2b 88 dd 83 54 9a ad f1 e9 84"))
    );
    assert_eq!(
        kv(&c_ap),
        (h("17 42 2d da 59 6e d5 d9 ac d8 90 e3 c6 3f 50 51"), h("5b 78 92 3d ee 08 57 90 33 e5 23 d9"))
    );
}

#[test]
fn record_protection() {
    // The server's handshake flight opens with the server handshake key, and is exactly
    // EncryptedExtensions..Finished plus the content type.
    let shared = h("8b d4 05 4f b5 5b 9d 63 fd fb ac f9 f0 4b 9f 0d 35 e6 d6 3f 53 75 63 ef d4 62 72 90 0f 89 49 2d");
    let mut t = Transcript::new();
    t.add(&h(CLIENT_HELLO));
    t.add(&h(SERVER_HELLO));
    let s = Schedule::handshake(CipherSuite::Aes128GcmSha256, &shared, &t.hash());
    let rec = h(SERVER_FLIGHT_RECORD);
    let mut body = rec[5..rec.len() - 16].to_vec();
    let tag: [u8; 16] = rec[rec.len() - 16..].try_into().unwrap();
    let mut rk = RecordKeys::new(CipherSuite::Aes128GcmSha256, &s.server_hs);
    assert!(rk.open(&rec[..5], &mut body, &tag));
    let mut want = h(ENCRYPTED_EXTENSIONS);
    want.extend(h(CERTIFICATE));
    want.extend(h(CERTIFICATE_VERIFY));
    want.extend(h(SERVER_FINISHED));
    want.push(22);
    assert_eq!(body, want);
    // Sealing the client Finished gives the RFC's record.
    let mut wk = RecordKeys::new(CipherSuite::Aes128GcmSha256, &s.client_hs);
    let mut data = h(CLIENT_FINISHED);
    data.push(22);
    let rec = h(CLIENT_FINISHED_RECORD);
    let tag = wk.seal(&rec[..5], &mut data).unwrap();
    data.extend_from_slice(&tag);
    assert_eq!(data, rec[5..]);
}

/// Hands out the RFC's server records and keeps what the client writes.
struct Script {
    incoming: Vec<u8>,
    at: usize,
    written: Vec<u8>,
}

impl Transport for Script {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        // Dribble it out in odd-sized pieces to exercise reassembly.
        let n = buf.len().min(self.incoming.len() - self.at).min(77);
        buf[..n].copy_from_slice(&self.incoming[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), IoError> {
        self.written.extend_from_slice(data);
        Ok(())
    }
}

fn record(ctype: u8, version: u16, body: &[u8]) -> Vec<u8> {
    let mut r = alloc::vec![ctype];
    r.extend_from_slice(&version.to_be_bytes());
    r.extend_from_slice(&(body.len() as u16).to_be_bytes());
    r.extend_from_slice(body);
    r
}

#[test]
fn full_handshake_replay() {
    let mut incoming = record(22, 0x0303, &h(SERVER_HELLO));
    incoming.extend(h(SERVER_FLIGHT_RECORD));
    incoming.extend(h(TICKET_RECORD));
    incoming.extend(h(SERVER_DATA_RECORD));
    incoming.extend(h(SERVER_ALERT_RECORD));
    let script = Script {
        incoming,
        at: 0,
        written: Vec::new(),
    };
    let mut cfg = Config::new(
        Arc::new(RootStore::empty()),
        Arc::new(|| Some(1_600_000_000u64)),
        Arc::new(|b: &mut [u8]| b.fill(0x42)),
    );
    cfg.cipher_suites = alloc::vec![CipherSuite::Aes128GcmSha256, CipherSuite::Chacha20Poly1305Sha256];
    cfg.policy = Policy {
        min_rsa_bits: 1024,
        max_rsa_bits: 4096,
    };
    cfg.test_hello = Some(TestHello {
        client_hello: h(CLIENT_HELLO),
        x25519_secret: h32(CLIENT_X25519_PRIVATE),
    });
    cfg.test_skip_certificate_checks = true;
    let client = Client::new(cfg);
    let mut s = client.connect("server", script).expect("handshake");
    assert_eq!(s.info().suite, CipherSuite::Aes128GcmSha256);
    assert_eq!(s.info().group, crate::NamedGroup::X25519);
    assert!(!s.info().hello_retry);
    // The client's flight: ClientHello (record version 0x0301), then the protected Finished.
    let mut want = record(22, 0x0301, &h(CLIENT_HELLO));
    want.extend(h(CLIENT_FINISHED_RECORD));
    assert_eq!(s.transport_mut().written, want);
    s.transport_mut().written.clear();

    let data: Vec<u8> = (0u8..50).collect();
    s.write_all(&data).unwrap();
    assert_eq!(s.transport_mut().written, h(CLIENT_DATA_RECORD));
    s.transport_mut().written.clear();

    let mut buf = [0u8; 100];
    let n = s.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], &data[..]);
    assert_eq!(s.tickets, 1);
    // The server's close_notify: end of stream.
    assert_eq!(s.read(&mut buf).unwrap(), 0);
    s.close().unwrap();
    assert_eq!(s.transport_mut().written, h(CLIENT_ALERT_RECORD));
}

#[test]
fn tampered_replay_fails_cleanly() {
    // Flip one bit anywhere in the server's protected flight: the handshake must fail with
    // Tampered (bad record MAC), never succeed or panic.
    let flight = h(SERVER_FLIGHT_RECORD);
    for i in (5..flight.len()).step_by(37) {
        let mut f = flight.clone();
        f[i] ^= 0x10;
        let mut incoming = record(22, 0x0303, &h(SERVER_HELLO));
        incoming.extend(f);
        let script = Script {
            incoming,
            at: 0,
            written: Vec::new(),
        };
        let mut cfg = Config::new(
            Arc::new(RootStore::empty()),
            Arc::new(|| Some(1_600_000_000u64)),
            Arc::new(|b: &mut [u8]| b.fill(0x42)),
        );
        cfg.policy.min_rsa_bits = 1024;
        cfg.test_hello = Some(TestHello {
            client_hello: h(CLIENT_HELLO),
            x25519_secret: h32(CLIENT_X25519_PRIVATE),
        });
        cfg.test_skip_certificate_checks = true;
        let r = Client::new(cfg).connect("server", script);
        assert!(matches!(r, Err(crate::TlsError::Tampered)), "byte {i}: {:?}", r.err());
    }
}
