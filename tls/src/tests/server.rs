//! Full handshakes against an in-process TLS 1.3 test server (a thread, over an in-memory
//! pipe). The server signs with the Ed25519 key of tls/testdata/chain/leaf-ed.der, and can
//! be told to ask for another group (HelloRetryRequest), send tickets and KeyUpdates,
//! fragment or coalesce its messages, ask for a client certificate, and misbehave in many
//! ways; the client must cope or fail with the right error, and never panic.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Duration;

use super::{NOW, Prng, fixture, mutate};
use crate::codec::{Reader, put_u8, put_u16, put_u24, with_len};
use crate::error::{Alert, CertError, IoError, TlsError};
use crate::keys::{self, CipherSuite, RecordKeys, Schedule, Transcript};
use crate::record::{self, Incoming, RecordLayer};
use crate::{Client, Config, NamedGroup, RootStore, Stream, Transport};
use milkcrypto::{ed25519, p256, p384, x25519};

// ---- an in-memory pipe ----------------------------------------------------------------

pub struct Pipe {
    rx: Receiver<Vec<u8>>,
    tx: Option<Sender<Vec<u8>>>,
    pending: Vec<u8>,
    at: usize,
}

fn pipe() -> (Pipe, Pipe) {
    let (a_tx, b_rx) = channel();
    let (b_tx, a_rx) = channel();
    (
        Pipe {
            rx: a_rx,
            tx: Some(a_tx),
            pending: Vec::new(),
            at: 0,
        },
        Pipe {
            rx: b_rx,
            tx: Some(b_tx),
            pending: Vec::new(),
            at: 0,
        },
    )
}

impl Pipe {
    fn close(&mut self) {
        self.tx = None;
    }
}

impl Transport for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, IoError> {
        if self.at >= self.pending.len() {
            match self.rx.recv_timeout(Duration::from_secs(5)) {
                Ok(v) => {
                    self.pending = v;
                    self.at = 0;
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(0),
                Err(RecvTimeoutError::Timeout) => return Err(IoError::TimedOut),
            }
        }
        let n = buf.len().min(self.pending.len() - self.at);
        buf[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), IoError> {
        match &self.tx {
            Some(tx) => tx
                .send(data.to_vec())
                .map_err(|_| IoError::Failed(String::from("broken pipe"))),
            None => Err(IoError::Failed(String::from("closed"))),
        }
    }
}

// ---- the test server ------------------------------------------------------------------

type Edit = Box<dyn FnMut(u8, &mut Vec<u8>) + Send>;

/// How the test server behaves.
pub struct Opts {
    pub suite: CipherSuite,
    pub group: NamedGroup,
    pub cookie: Option<Vec<u8>>,
    pub alpn: Option<Vec<u8>>,
    pub chain: Vec<Vec<u8>>,
    pub tickets: usize,
    pub cert_request: bool,
    /// After the first application data: send KeyUpdate(update_requested).
    pub key_update: bool,
    /// Handshake fragment size for the protected flight (0 = one record per message,
    /// usize::MAX = everything in one record).
    pub fragment: usize,
    /// Edit each handshake message (type, whole message) before it is sent and hashed.
    pub edit: Option<Edit>,
    /// Extra EncryptedExtensions entries.
    pub extra_ee: Vec<(u16, Vec<u8>)>,
    pub misbehave: Misbehave,
    /// Echo application data back, then expect close_notify.
    pub echo: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Misbehave {
    None,
    Tls12ServerHello,
    AlertAfterHello(u8),
    TwoRetries,
    RetrySameGroup,
    RetryNoChange,
    SuiteChangeAfterRetry,
    WrongSessionId,
    UnofferedSuite,
    PreSharedKey,
    ZeroKeyShare,
    WrongShareGroup,
    BadSignature,
    BadFinished,
    AppDataBeforeFinished,
    CloseInHandshake,
    CloseAfterHandshake,
    OversizeRecord,
    CcsAfterHandshake,
    KeyUpdateNotLast,
    PostHandshakeCertRequest,
    LongTicket,
    EmptyCertificate,
    RecordBitFlip,
    UnknownRecordType,
    PlaintextAfterKeys,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            suite: CipherSuite::Chacha20Poly1305Sha256,
            group: NamedGroup::X25519,
            cookie: None,
            alpn: None,
            chain: vec![fixture("leaf-ed")],
            tickets: 0,
            cert_request: false,
            key_update: false,
            fragment: 0,
            edit: None,
            extra_ee: Vec::new(),
            misbehave: Misbehave::None,
            echo: true,
        }
    }
}

/// What the server saw.
#[derive(Default, Debug)]
pub struct Seen {
    pub sni: Option<String>,
    pub alert: Option<u8>,
    pub echoed: usize,
    pub client_key_updates: usize,
    pub client_hellos: usize,
    pub client_sent_empty_certificate: bool,
    pub error: Option<String>,
}

fn seed() -> [u8; 32] {
    let s = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/chain/leaf-ed.TEST-ONLY-seed"
    ))
    .unwrap();
    milkcrypto::unhex(s.trim()).unwrap()
}

fn hs(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![kind];
    put_u24(&mut m, body.len() as u32);
    m.extend_from_slice(body);
    m
}

const HRR_RANDOM: [u8; 32] = [
    0xcf, 0x21, 0xad, 0x74, 0xe5, 0x9a, 0x61, 0x11, 0xbe, 0x1d, 0x8c, 0x02, 0x1e, 0x65, 0xb8, 0x91,
    0xc2, 0xa2, 0x11, 0x16, 0x7a, 0xbb, 0x8c, 0x5e, 0x07, 0x9e, 0x09, 0xe2, 0xc8, 0xa8, 0x33, 0x9c,
];

struct Hello {
    session_id: Vec<u8>,
    shares: Vec<(u16, Vec<u8>)>,
    sni: Option<String>,
    alpn: Vec<Vec<u8>>,
    cookie: Option<Vec<u8>>,
    suites: Vec<u16>,
}

fn parse_hello(m: &[u8]) -> Option<Hello> {
    let mut r = Reader::new(&m[4..]);
    r.u16()?;
    r.take(32)?;
    let session_id = r.vec8()?.to_vec();
    let mut s = Reader::new(r.vec16()?);
    let mut suites = Vec::new();
    while let Some(x) = s.u16() {
        suites.push(x);
    }
    r.vec8()?;
    let mut e = Reader::new(r.vec16()?);
    let mut h = Hello {
        session_id,
        shares: Vec::new(),
        sni: None,
        alpn: Vec::new(),
        cookie: None,
        suites,
    };
    while !e.is_empty() {
        let t = e.u16()?;
        let v = e.vec16()?;
        let mut d = Reader::new(v);
        match t {
            0 => {
                let mut l = Reader::new(d.vec16()?);
                l.u8()?;
                h.sni = Some(String::from_utf8(l.vec16()?.to_vec()).ok()?);
            }
            16 => {
                let mut l = Reader::new(d.vec16()?);
                while !l.is_empty() {
                    h.alpn.push(l.vec8()?.to_vec());
                }
            }
            44 => h.cookie = Some(d.vec16()?.to_vec()),
            51 => {
                let mut l = Reader::new(d.vec16()?);
                while !l.is_empty() {
                    let g = l.u16()?;
                    h.shares.push((g, l.vec16()?.to_vec()));
                }
            }
            _ => {}
        }
    }
    Some(h)
}

fn server_share(group: NamedGroup, peer: &[u8], r: &mut Prng) -> (Vec<u8>, Vec<u8>) {
    match group {
        NamedGroup::X25519 => {
            let mut s = [0u8; 32];
            r.fill(&mut s);
            let p: [u8; 32] = peer.try_into().unwrap();
            (x25519::public_key(&s).to_vec(), x25519::shared(&s, &p).unwrap().to_vec())
        }
        NamedGroup::Secp256r1 => {
            let mut s = [0u8; 32];
            r.fill(&mut s);
            s[0] &= 0x7f;
            (p256::public_key(&s).unwrap().to_vec(), p256::shared(&s, peer).unwrap().to_vec())
        }
        NamedGroup::Secp384r1 => {
            let mut s = [0u8; 48];
            r.fill(&mut s);
            s[0] &= 0x7f;
            (p384::public_key(&s).unwrap().to_vec(), p384::shared(&s, peer).unwrap().to_vec())
        }
    }
}

fn group_of(id: u16) -> Option<NamedGroup> {
    [NamedGroup::X25519, NamedGroup::Secp256r1, NamedGroup::Secp384r1]
        .into_iter()
        .find(|g| g.id() == id)
}

fn server_hello(random: &[u8; 32], sid: &[u8], suite: u16, exts: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    put_u16(&mut b, 0x0303);
    b.extend_from_slice(random);
    with_len(&mut b, 1, |o| o.extend_from_slice(sid));
    put_u16(&mut b, suite);
    put_u8(&mut b, 0);
    with_len(&mut b, 2, |o| o.extend_from_slice(exts));
    hs(2, &b)
}

fn ext(out: &mut Vec<u8>, t: u16, v: &[u8]) {
    put_u16(out, t);
    with_len(out, 2, |o| o.extend_from_slice(v));
}

fn read_hs(rl: &mut RecordLayer<Pipe>, seen: &mut Seen) -> Result<Vec<u8>, TlsError> {
    loop {
        if let Some(m) = rl.take_handshake()? {
            return Ok(m);
        }
        match rl.read()? {
            Incoming::Handshake => {}
            Incoming::Alert(a) => {
                seen.alert = Some(a.0);
                return Err(TlsError::PeerAlert(a));
            }
            Incoming::ApplicationData(_) => return Err(TlsError::unexpected("data")),
        }
    }
}

/// Run one server connection. Returns what it saw (errors included).
fn serve(mut o: Opts, t: Pipe) -> Seen {
    let mut seen = Seen::default();
    let mut rl = RecordLayer::new(t);
    rl.write_version = 0x0303;
    if let Err(e) = serve_inner(&mut o, &mut rl, &mut seen) {
        seen.error = Some(alloc::format!("{e:?}"));
    }
    // Drain: note an alert the client sends after our last message.
    if seen.alert.is_none() {
        while let Ok(i) = rl.read() {
            if let Incoming::Alert(a) = i {
                seen.alert = Some(a.0);
                break;
            }
        }
    }
    seen
}

fn send_flight(rl: &mut RecordLayer<Pipe>, msgs: &[Vec<u8>], fragment: usize) -> Result<(), TlsError> {
    if fragment == usize::MAX {
        let all: Vec<u8> = msgs.concat();
        return rl.write(record::CT_HANDSHAKE, &all);
    }
    for m in msgs {
        if fragment == 0 {
            rl.write(record::CT_HANDSHAKE, m)?;
        } else {
            for c in m.chunks(fragment) {
                rl.write(record::CT_HANDSHAKE, c)?;
            }
        }
    }
    Ok(())
}

fn serve_inner(o: &mut Opts, rl: &mut RecordLayer<Pipe>, seen: &mut Seen) -> Result<(), TlsError> {
    let mut rng = Prng(0x7e57_5e4e);
    let mut transcript = Transcript::new();
    let mut ch = read_hs(rl, seen)?;
    seen.client_hellos += 1;
    let mut hello = parse_hello(&ch).ok_or(TlsError::decode("ClientHello"))?;
    seen.sni = hello.sni.clone();
    transcript.add(&ch);
    let mut random = [0u8; 32];
    rng.fill(&mut random);
    let sid = if o.misbehave == Misbehave::WrongSessionId {
        vec![9u8; 32]
    } else {
        hello.session_id.clone()
    };
    let suite_id = match o.misbehave {
        Misbehave::UnofferedSuite => 0x1302,
        _ => o.suite.id(),
    };
    match o.misbehave {
        Misbehave::Tls12ServerHello => {
            let mut b = Vec::new();
            put_u16(&mut b, 0x0303);
            let mut r = random;
            r[24..].copy_from_slice(b"DOWNGRD\x01");
            b.extend_from_slice(&r);
            with_len(&mut b, 1, |o| o.extend_from_slice(&sid));
            put_u16(&mut b, 0xc02f);
            put_u8(&mut b, 0);
            with_len(&mut b, 2, |o| ext(o, 0xff01, &[0]));
            rl.write(record::CT_HANDSHAKE, &hs(2, &b))?;
            return Ok(());
        }
        Misbehave::AlertAfterHello(a) => {
            rl.write(record::CT_ALERT, &[2, a])?;
            return Ok(());
        }
        Misbehave::CloseInHandshake => {
            rl.transport.close();
            return Ok(());
        }
        _ => {}
    }

    // HelloRetryRequest when the client has no share for our group.
    let mut retried = false;
    loop {
        let have = hello.shares.iter().any(|(g, _)| *g == o.group.id());
        let force = matches!(
            o.misbehave,
            Misbehave::RetrySameGroup | Misbehave::RetryNoChange | Misbehave::TwoRetries
        ) && !retried;
        let again = o.misbehave == Misbehave::TwoRetries && retried && seen.client_hellos == 2;
        if have && !force && !again {
            break;
        }
        let mut e = Vec::new();
        ext(&mut e, 43, &[0x03, 0x04]);
        match o.misbehave {
            Misbehave::RetryNoChange => {}
            Misbehave::RetrySameGroup => ext(&mut e, 51, &hello.shares[0].0.to_be_bytes()),
            _ => ext(&mut e, 51, &o.group.id().to_be_bytes()),
        }
        if let Some(c) = &o.cookie {
            let mut v = Vec::new();
            with_len(&mut v, 2, |x| x.extend_from_slice(c));
            ext(&mut e, 44, &v);
        }
        let hrr = server_hello(&HRR_RANDOM, &sid, suite_id, &e);
        transcript.restart_for_retry();
        transcript.add(&hrr);
        rl.write(record::CT_HANDSHAKE, &hrr)?;
        retried = true;
        ch = read_hs(rl, seen)?;
        seen.client_hellos += 1;
        hello = parse_hello(&ch).ok_or(TlsError::decode("ClientHello 2"))?;
        if hello.cookie != o.cookie {
            return Err(TlsError::illegal("cookie not echoed"));
        }
        transcript.add(&ch);
    }
    let _ = &hello.suites;

    let (group_id, peer) = hello
        .shares
        .iter()
        .find(|(g, _)| *g == o.group.id())
        .cloned()
        .ok_or(TlsError::illegal("no share"))?;
    let (mut public, shared) = server_share(group_of(group_id).unwrap(), &peer, &mut rng);
    let mut share_group = group_id;
    if o.misbehave == Misbehave::ZeroKeyShare {
        public = vec![0u8; 32];
    }
    if o.misbehave == Misbehave::WrongShareGroup {
        share_group = NamedGroup::Secp384r1.id();
    }
    let mut e = Vec::new();
    ext(&mut e, 43, &[0x03, 0x04]);
    let mut ks = Vec::new();
    put_u16(&mut ks, share_group);
    with_len(&mut ks, 2, |x| x.extend_from_slice(&public));
    ext(&mut e, 51, &ks);
    if o.misbehave == Misbehave::PreSharedKey {
        ext(&mut e, 41, &[0, 0]);
    }
    let sh_suite = if o.misbehave == Misbehave::SuiteChangeAfterRetry {
        CipherSuite::Aes128GcmSha256.id()
    } else {
        suite_id
    };
    let mut sh = server_hello(&random, &sid, sh_suite, &e);
    if let Some(f) = &mut o.edit {
        f(2, &mut sh);
    }
    transcript.add(&sh);
    rl.write(record::CT_HANDSHAKE, &sh)?;
    rl.write_plain(record::CT_CHANGE_CIPHER_SPEC, &[1])?;
    let suite = o.suite;
    let sched = Schedule::handshake(suite, &shared, &transcript.hash());
    rl.write_keys = Some(RecordKeys::new(suite, &sched.server_hs));
    rl.read_keys = Some(RecordKeys::new(suite, &sched.client_hs));

    let mut flight: Vec<Vec<u8>> = Vec::new();
    let mut push = |kind: u8, mut m: Vec<u8>, transcript: &mut Transcript, flight: &mut Vec<Vec<u8>>| {
        if let Some(f) = &mut o.edit {
            f(kind, &mut m);
        }
        transcript.add(&m);
        flight.push(m);
    };
    // EncryptedExtensions.
    let mut ee = Vec::new();
    if let Some(p) = &o.alpn {
        let mut v = Vec::new();
        with_len(&mut v, 2, |x| with_len(x, 1, |y| y.extend_from_slice(p)));
        ext(&mut ee, 16, &v);
    }
    if hello.sni.is_some() {
        ext(&mut ee, 0, &[]);
    }
    for (t, v) in &o.extra_ee {
        ext(&mut ee, *t, v);
    }
    let mut b = Vec::new();
    with_len(&mut b, 2, |x| x.extend_from_slice(&ee));
    push(8, hs(8, &b), &mut transcript, &mut flight);
    if o.cert_request {
        let mut b = Vec::new();
        with_len(&mut b, 1, |x| x.extend_from_slice(b"ctx"));
        with_len(&mut b, 2, |x| {
            let mut sa = Vec::new();
            with_len(&mut sa, 2, |y| put_u16(y, 0x0807));
            ext(x, 13, &sa);
        });
        push(13, hs(13, &b), &mut transcript, &mut flight);
    }
    // Certificate.
    let mut b = Vec::new();
    with_len(&mut b, 1, |_| {});
    with_len(&mut b, 3, |x| {
        if o.misbehave != Misbehave::EmptyCertificate {
            for c in &o.chain {
                with_len(x, 3, |y| y.extend_from_slice(c));
                with_len(x, 2, |_| {});
            }
        }
    });
    push(11, hs(11, &b), &mut transcript, &mut flight);
    // CertificateVerify (Ed25519).
    let mut signed = vec![0x20u8; 64];
    signed.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
    signed.extend_from_slice(&transcript.hash());
    if o.misbehave == Misbehave::BadSignature {
        signed[70] ^= 1;
    }
    let sig = ed25519::sign(&seed(), &signed);
    let mut b = Vec::new();
    put_u16(&mut b, 0x0807);
    with_len(&mut b, 2, |x| x.extend_from_slice(&sig));
    push(15, hs(15, &b), &mut transcript, &mut flight);
    // Finished.
    let mut fin = keys::finished_verify_data(&sched.server_hs, &transcript.hash());
    if o.misbehave == Misbehave::BadFinished {
        fin[0] ^= 1;
    }
    push(20, hs(20, &fin), &mut transcript, &mut flight);
    if o.misbehave == Misbehave::AppDataBeforeFinished {
        let last = flight.pop().unwrap();
        send_flight(rl, &flight, o.fragment)?;
        rl.write(record::CT_APPLICATION_DATA, b"early")?;
        rl.write(record::CT_HANDSHAKE, &last)?;
    } else if o.misbehave == Misbehave::PlaintextAfterKeys {
        rl.write_plain(record::CT_HANDSHAKE, &flight[0])?;
    } else if o.misbehave == Misbehave::RecordBitFlip {
        // Seal normally, then flip a bit in the record on the wire.
        let all: Vec<u8> = flight.concat();
        let mut wire = Vec::new();
        {
            let k = rl.write_keys.as_mut().unwrap();
            let len = all.len() + 17;
            let header = [23u8, 3, 3, (len >> 8) as u8, len as u8];
            let mut body = all.clone();
            body.push(22);
            let tag = k.seal(&header, &mut body).unwrap();
            wire.extend_from_slice(&header);
            wire.extend_from_slice(&body);
            wire.extend_from_slice(&tag);
        }
        let i = 5 + rng.below(wire.len() - 5);
        wire[i] ^= 1 << rng.below(8);
        rl.transport.write_all(&wire)?;
    } else {
        send_flight(rl, &flight, o.fragment)?;
    }
    let (c_ap, s_ap) = sched.application(&transcript.hash());

    // The client's flight: [CCS, dropped by the record layer] [Certificate] Finished.
    let mut m = read_hs(rl, seen)?;
    if o.cert_request {
        if m[0] != 11 || m[4..] != [3, b'c', b't', b'x', 0, 0, 0] {
            return Err(TlsError::unexpected("client Certificate"));
        }
        seen.client_sent_empty_certificate = true;
        transcript.add(&m);
        m = read_hs(rl, seen)?;
    }
    let want = keys::finished_verify_data(&sched.client_hs, &transcript.hash());
    if m[0] != 20 || m[4..] != want {
        return Err(TlsError::Tampered);
    }
    let mut s_secret = s_ap;
    let mut c_secret = c_ap;
    rl.write_keys = Some(RecordKeys::new(suite, &s_secret));
    rl.read_keys = Some(RecordKeys::new(suite, &c_secret));
    rl.allow_ccs = false;
    rl.established = true;

    match o.misbehave {
        Misbehave::CloseAfterHandshake => {
            rl.transport.close();
            return Ok(());
        }
        Misbehave::OversizeRecord => {
            let mut r = vec![23u8, 3, 3, 0x41, 0x01];
            r.resize(5 + 0x4101, 0);
            rl.transport.write_all(&r)?;
            return Ok(());
        }
        Misbehave::CcsAfterHandshake => {
            rl.write_plain(record::CT_CHANGE_CIPHER_SPEC, &[1])?;
            return Ok(());
        }
        Misbehave::UnknownRecordType => {
            rl.write_plain(99, &[1, 2, 3])?;
            return Ok(());
        }
        Misbehave::KeyUpdateNotLast => {
            let mut m = hs(24, &[0]);
            m.extend_from_slice(&hs(4, &[0; 3])[..3]);
            rl.write(record::CT_HANDSHAKE, &m)?;
            return Ok(());
        }
        Misbehave::PostHandshakeCertRequest => {
            let mut b = Vec::new();
            with_len(&mut b, 1, |x| x.push(1));
            with_len(&mut b, 2, |_| {});
            rl.write(record::CT_HANDSHAKE, &hs(13, &b))?;
            return Ok(());
        }
        Misbehave::LongTicket => {
            let mut b = Vec::new();
            b.extend_from_slice(&604_801u32.to_be_bytes());
            b.extend_from_slice(&[0; 4]);
            with_len(&mut b, 1, |_| {});
            with_len(&mut b, 2, |x| x.push(1));
            with_len(&mut b, 2, |_| {});
            rl.write(record::CT_HANDSHAKE, &hs(4, &b))?;
            return Ok(());
        }
        _ => {}
    }
    for i in 0..o.tickets {
        let mut b = Vec::new();
        b.extend_from_slice(&7200u32.to_be_bytes());
        b.extend_from_slice(&[1, 2, 3, 4]);
        with_len(&mut b, 1, |x| x.push(i as u8));
        with_len(&mut b, 2, |x| x.extend_from_slice(&[0xab; 100]));
        with_len(&mut b, 2, |_| {});
        rl.write(record::CT_HANDSHAKE, &hs(4, &b))?;
    }
    if !o.echo {
        return Ok(());
    }
    let mut first = true;
    loop {
        match rl.read()? {
            Incoming::ApplicationData(d) => {
                if first && o.key_update {
                    rl.write(record::CT_HANDSHAKE, &hs(24, &[1]))?;
                    s_secret = keys::next_traffic_secret(&s_secret);
                    rl.write_keys = Some(RecordKeys::new(suite, &s_secret));
                }
                first = false;
                seen.echoed += d.len();
                rl.write(record::CT_APPLICATION_DATA, &d)?;
            }
            Incoming::Handshake => {
                while let Some(m) = rl.take_handshake()? {
                    if m[0] != 24 {
                        return Err(TlsError::unexpected("client post-handshake"));
                    }
                    seen.client_key_updates += 1;
                    c_secret = keys::next_traffic_secret(&c_secret);
                    rl.read_keys = Some(RecordKeys::new(suite, &c_secret));
                    if m[4] == 1 {
                        rl.write(record::CT_HANDSHAKE, &hs(24, &[0]))?;
                        s_secret = keys::next_traffic_secret(&s_secret);
                        rl.write_keys = Some(RecordKeys::new(suite, &s_secret));
                    }
                }
            }
            Incoming::Alert(a) => {
                seen.alert = Some(a.0);
                if a == Alert::CLOSE_NOTIFY {
                    // The client may already be gone: answering is best effort.
                    let _ = rl.write(record::CT_ALERT, &[1, 0]);
                }
                return Ok(());
            }
        }
    }
}

// ---- the client side ------------------------------------------------------------------

fn config(roots: &[&str]) -> Config {
    let mut s = RootStore::empty();
    for r in roots {
        s.add_der(&fixture(r)).unwrap();
    }
    let counter = Arc::new(Mutex::new(Prng(0xc11e_47)));
    Config::new(
        Arc::new(s),
        Arc::new(|| Some(NOW as u64)),
        Arc::new(move |b: &mut [u8]| counter.lock().unwrap().fill(b)),
    )
}

/// Connect `cfg` to a server with `opts`; run `f` on the stream; return the client result
/// and what the server saw.
fn run<R>(
    cfg: Config,
    host: &str,
    opts: Opts,
    f: impl FnOnce(&mut Stream<Pipe>) -> R,
) -> (Result<R, TlsError>, Seen) {
    let (c, s) = pipe();
    let server = std::thread::spawn(move || serve(opts, s));
    let client = Client::new(cfg);
    let r = match client.connect(host, c) {
        Ok(mut st) => {
            let out = f(&mut st);
            drop(st);
            Ok(out)
        }
        Err(e) => Err(e),
    };
    let seen = server.join().expect("server thread");
    (r, seen)
}

/// Write, read back the echo, close.
fn echo(st: &mut Stream<Pipe>, data: &[u8]) -> Result<Vec<u8>, TlsError> {
    st.write_all(data)?;
    let mut got = Vec::new();
    let mut buf = vec![0u8; 4096];
    while got.len() < data.len() {
        let n = st.read(&mut buf)?;
        if n == 0 {
            break;
        }
        got.extend_from_slice(&buf[..n]);
    }
    st.close()?;
    // The server answers close_notify with its own.
    let mut tail = [0u8; 1];
    let _ = st.read(&mut tail);
    Ok(got)
}

const HOST: &str = "test.milk.example";

#[test]
fn basic_handshake_both_suites() {
    for suite in [CipherSuite::Chacha20Poly1305Sha256, CipherSuite::Aes128GcmSha256] {
        let opts = Opts {
            suite,
            alpn: Some(b"http/1.1".to_vec()),
            ..Opts::default()
        };
        let data: Vec<u8> = (0..40_000u32).map(|i| (i * 7) as u8).collect();
        let (r, seen) = run(config(&["root-ed"]), HOST, opts, |st| {
            assert_eq!(st.info().suite, suite);
            assert_eq!(st.info().group, NamedGroup::X25519);
            assert_eq!(st.info().alpn.as_deref(), Some(&b"http/1.1"[..]));
            assert_eq!(st.info().summary(), alloc::format!("TLS 1.3, {}, X25519", suite.name()));
            assert_eq!(st.info().issuer().as_deref(), Some("milkOS tests"));
            echo(st, &data)
        });
        assert_eq!(r.unwrap().unwrap(), data);
        assert_eq!(seen.sni.as_deref(), Some(HOST));
        assert_eq!(seen.echoed, data.len());
        assert_eq!(seen.alert, Some(0), "close_notify");
        assert!(seen.error.is_none(), "{:?}", seen.error);
    }
}

#[test]
fn ip_address_and_no_sni() {
    let (r, seen) = run(config(&["root-ed"]), "127.0.0.1", Opts::default(), |st| echo(st, b"hi"));
    assert_eq!(r.unwrap().unwrap(), b"hi");
    assert_eq!(seen.sni, None);
}

#[test]
fn hello_retry_request() {
    for (group, cookie) in [
        (NamedGroup::Secp256r1, None),
        (NamedGroup::Secp384r1, None),
        (NamedGroup::Secp256r1, Some(b"a cookie".to_vec())),
    ] {
        let opts = Opts {
            group,
            cookie: cookie.clone(),
            ..Opts::default()
        };
        let (r, seen) = run(config(&["root-ed"]), HOST, opts, |st| {
            assert!(st.info().hello_retry);
            assert_eq!(st.info().group, group);
            echo(st, b"after a retry")
        });
        assert_eq!(r.unwrap().unwrap(), b"after a retry");
        assert_eq!(seen.client_hellos, 2);
        assert!(seen.error.is_none(), "{:?}", seen.error);
    }
    // A cookie alone (same group) is a change too.
    let opts = Opts {
        cookie: Some(b"c".to_vec()),
        misbehave: Misbehave::RetryNoChange,
        ..Opts::default()
    };
    let (r, seen) = run(config(&["root-ed"]), HOST, opts, |st| echo(st, b"x"));
    assert_eq!(r.unwrap().unwrap(), b"x");
    assert_eq!(seen.client_hellos, 2);
    // The client can start with P-256 too.
    let mut cfg = config(&["root-ed"]);
    cfg.groups = vec![NamedGroup::Secp256r1, NamedGroup::X25519];
    let opts = Opts {
        group: NamedGroup::Secp256r1,
        ..Opts::default()
    };
    let (r, seen) = run(cfg, HOST, opts, |st| {
        assert!(!st.info().hello_retry);
        echo(st, b"p256")
    });
    assert_eq!(r.unwrap().unwrap(), b"p256");
    assert_eq!(seen.client_hellos, 1);
}

fn expect_err(opts: Opts, want: TlsError) -> Seen {
    expect_err_cfg(config(&["root-ed"]), HOST, opts, want)
}

fn expect_err_cfg(cfg: Config, host: &str, opts: Opts, want: TlsError) -> Seen {
    let (r, seen) = run(cfg, host, opts, |st| echo(st, b"data"));
    let e = match r {
        Ok(Ok(_)) => panic!("expected {want:?}, got a working connection"),
        Ok(Err(e)) | Err(e) => e,
    };
    assert_eq!(e, want);
    seen
}

#[test]
fn retry_misbehaviour() {
    let m = |misbehave, group| Opts {
        misbehave,
        group,
        ..Opts::default()
    };
    expect_err(
        m(Misbehave::RetrySameGroup, NamedGroup::X25519),
        TlsError::illegal("a HelloRetryRequest for the key share we sent"),
    );
    expect_err(
        m(Misbehave::RetryNoChange, NamedGroup::X25519),
        TlsError::illegal("a HelloRetryRequest that changes nothing"),
    );
    expect_err(m(Misbehave::TwoRetries, NamedGroup::Secp256r1), TlsError::unexpected("a second HelloRetryRequest"));
    expect_err(
        m(Misbehave::SuiteChangeAfterRetry, NamedGroup::Secp256r1),
        TlsError::illegal("the cipher suite changed after HelloRetryRequest"),
    );
    // A group the client doesn't offer.
    let mut cfg = config(&["root-ed"]);
    cfg.groups = vec![NamedGroup::X25519];
    let seen = expect_err_cfg(
        cfg,
        HOST,
        m(Misbehave::None, NamedGroup::Secp256r1),
        TlsError::illegal("a HelloRetryRequest for a group we don't support"),
    );
    assert_eq!(seen.alert, Some(47));
}

#[test]
fn key_update_both_ways() {
    // The server asks for an update after the first data.
    let opts = Opts {
        key_update: true,
        ..Opts::default()
    };
    let (r, seen) = run(config(&["root-ed"]), HOST, opts, |st| {
        st.write_all(b"one")?;
        let mut b = [0u8; 16];
        let n = st.read(&mut b)?;
        assert_eq!(&b[..n], b"one");
        assert_eq!(st.key_updates_received, 1);
        st.write_all(b"two")?;
        let n = st.read(&mut b)?;
        assert_eq!(&b[..n], b"two");
        // Now we ask.
        st.key_update()?;
        st.write_all(b"three")?;
        let n = st.read(&mut b)?;
        assert_eq!(&b[..n], b"three");
        assert_eq!(st.key_updates_received, 2);
        st.close()
    });
    r.unwrap().unwrap();
    assert_eq!(seen.client_key_updates, 2, "the answer to the server's, then ours");
    assert!(seen.error.is_none(), "{:?}", seen.error);
    // Automatic: rekey every 3 records.
    let mut cfg = config(&["root-ed"]);
    cfg.rekey_after_records = 3;
    let data = vec![0x5au8; 16384 * 10 + 5];
    let (r, seen) = run(cfg, HOST, Opts::default(), |st| echo(st, &data));
    assert_eq!(r.unwrap().unwrap(), data);
    assert!(seen.client_key_updates >= 3, "{}", seen.client_key_updates);
}

#[test]
fn tickets_fragments_and_client_certificate_requests() {
    let opts = Opts {
        tickets: 3,
        fragment: 1,
        ..Opts::default()
    };
    let (r, _) = run(config(&["root-ed"]), HOST, opts, |st| {
        let out = echo(st, b"fragmented");
        assert_eq!(st.tickets, 3);
        out
    });
    assert_eq!(r.unwrap().unwrap(), b"fragmented");
    for fragment in [7, 300, usize::MAX] {
        let opts = Opts {
            fragment,
            ..Opts::default()
        };
        let (r, _) = run(config(&["root-ed"]), HOST, opts, |st| echo(st, b"ok"));
        assert_eq!(r.unwrap().unwrap(), b"ok");
    }
    let opts = Opts {
        cert_request: true,
        ..Opts::default()
    };
    let (r, seen) = run(config(&["root-ed"]), HOST, opts, |st| echo(st, b"no cert"));
    assert_eq!(r.unwrap().unwrap(), b"no cert");
    assert!(seen.client_sent_empty_certificate);
}

#[test]
fn certificate_errors_send_alerts() {
    let seen = expect_err_cfg(
        config(&["root-ed"]),
        "other.example",
        Opts::default(),
        TlsError::Certificate(CertError::WrongHost),
    );
    assert_eq!(seen.alert, Some(46));
    let seen = expect_err_cfg(config(&["root-ec"]), HOST, Opts::default(), TlsError::Certificate(CertError::UnknownIssuer));
    assert_eq!(seen.alert, Some(48));
    let mut cfg = config(&["root-ed"]);
    cfg.clock = Arc::new(|| Some(2_400_000_000u64));
    let seen = expect_err_cfg(cfg, HOST, Opts::default(), TlsError::Certificate(CertError::Expired));
    assert_eq!(seen.alert, Some(45));
    let mut cfg = config(&["root-ed"]);
    cfg.clock = Arc::new(|| None);
    expect_err_cfg(cfg, HOST, Opts::default(), TlsError::ClockNotSet);
    let mut cfg = config(&["root-ed"]);
    cfg.clock = Arc::new(|| Some(1000));
    expect_err_cfg(cfg, HOST, Opts::default(), TlsError::ClockNotSet);
    let seen = expect_err(
        Opts {
            misbehave: Misbehave::EmptyCertificate,
            ..Opts::default()
        },
        TlsError::Certificate(CertError::NoCertificate),
    );
    assert_eq!(seen.alert, Some(50));
    // A chain that doesn't end at our anchor.
    let opts = Opts {
        chain: vec![fixture("leaf-ec"), fixture("int-ec")],
        ..Opts::default()
    };
    expect_err(opts, TlsError::Certificate(CertError::UnknownIssuer));
    assert_eq!(
        Client::new(config(&["root-ed"])).connect("bad name!", pipe().0).err(),
        Some(TlsError::InvalidServerName)
    );
}

#[test]
fn signature_and_finished_must_verify() {
    let seen = expect_err(
        Opts {
            misbehave: Misbehave::BadSignature,
            ..Opts::default()
        },
        TlsError::BadHandshakeSignature,
    );
    assert_eq!(seen.alert, Some(51));
    let seen = expect_err(
        Opts {
            misbehave: Misbehave::BadFinished,
            ..Opts::default()
        },
        TlsError::Tampered,
    );
    assert_eq!(seen.alert, Some(20));
    // The ECDSA leaf with an Ed25519 signature: the scheme doesn't fit the key.
    let opts = Opts {
        chain: vec![fixture("leaf-ec"), fixture("int-ec")],
        ..Opts::default()
    };
    expect_err_cfg(
        config(&["root-ec"]),
        HOST,
        opts,
        TlsError::illegal("a signature scheme that doesn't fit the certificate"),
    );
}

#[test]
fn protocol_errors() {
    let m = |misbehave| Opts {
        misbehave,
        ..Opts::default()
    };
    expect_err(m(Misbehave::Tls12ServerHello), TlsError::NotTls13);
    expect_err(m(Misbehave::AlertAfterHello(70)), TlsError::NotTls13);
    expect_err(m(Misbehave::AlertAfterHello(40)), TlsError::NoCommonAlgorithms);
    expect_err(m(Misbehave::AlertAfterHello(80)), TlsError::PeerAlert(Alert::INTERNAL_ERROR));
    expect_err(m(Misbehave::AlertAfterHello(0)), TlsError::ConnectionClosed);
    expect_err(m(Misbehave::CloseInHandshake), TlsError::ConnectionClosed);
    expect_err(m(Misbehave::WrongSessionId), TlsError::illegal("the ServerHello didn't echo our session id"));
    expect_err(m(Misbehave::UnofferedSuite), TlsError::illegal("a cipher suite we didn't offer"));
    expect_err(
        m(Misbehave::PreSharedKey),
        TlsError::proto(Alert::UNSUPPORTED_EXTENSION, "a pre-shared key we didn't offer"),
    );
    expect_err(m(Misbehave::ZeroKeyShare), TlsError::illegal("an invalid key share"));
    expect_err(m(Misbehave::WrongShareGroup), TlsError::illegal("a key share for a group we didn't send"));
    expect_err(m(Misbehave::AppDataBeforeFinished), TlsError::unexpected("application data during the handshake"));
    expect_err(m(Misbehave::PlaintextAfterKeys), TlsError::unexpected("an unprotected record after the keys changed"));
    expect_err(m(Misbehave::RecordBitFlip), TlsError::Tampered);
    // ALPN the client didn't offer; an extension it didn't ask for.
    expect_err(
        Opts {
            alpn: Some(b"h2".to_vec()),
            ..Opts::default()
        },
        TlsError::NoApplicationProtocol,
    );
    expect_err(
        Opts {
            extra_ee: vec![(0x1234, vec![])],
            ..Opts::default()
        },
        TlsError::proto(Alert::UNSUPPORTED_EXTENSION, "an extension we didn't ask for"),
    );
    expect_err(
        Opts {
            extra_ee: vec![(43, vec![3, 4])],
            ..Opts::default()
        },
        TlsError::illegal("an extension that doesn't belong in EncryptedExtensions"),
    );
}

#[test]
fn after_the_handshake() {
    let m = |misbehave| Opts {
        misbehave,
        ..Opts::default()
    };
    let read_err = |misbehave| {
        let (r, seen) = run(config(&["root-ed"]), HOST, m(misbehave), |st| {
            let mut b = [0u8; 8];
            st.read(&mut b)
        });
        (r.unwrap().unwrap_err(), seen)
    };
    assert_eq!(read_err(Misbehave::CloseAfterHandshake).0, TlsError::Truncated);
    assert_eq!(
        read_err(Misbehave::OversizeRecord).0,
        TlsError::proto(Alert::RECORD_OVERFLOW, "a record was too long")
    );
    assert_eq!(read_err(Misbehave::CcsAfterHandshake).0, TlsError::unexpected("a change_cipher_spec record"));
    assert_eq!(read_err(Misbehave::UnknownRecordType).0, TlsError::unexpected("a record of unknown type"));
    assert_eq!(
        read_err(Misbehave::KeyUpdateNotLast).0,
        TlsError::unexpected("handshake data across a key change")
    );
    let (e, seen) = read_err(Misbehave::PostHandshakeCertRequest);
    assert_eq!(e, TlsError::unexpected("a post-handshake CertificateRequest"));
    assert_eq!(seen.alert, Some(10));
    assert_eq!(read_err(Misbehave::LongTicket).0, TlsError::illegal("a session ticket lifetime over 7 days"));
    // After an error the stream stays dead.
    let (r, _) = run(config(&["root-ed"]), HOST, m(Misbehave::CloseAfterHandshake), |st| {
        let mut b = [0u8; 8];
        let first = st.read(&mut b);
        (first, st.read(&mut b), st.write_all(b"x"))
    });
    let (a, b, c) = r.unwrap();
    assert_eq!(a, Err(TlsError::Truncated));
    assert_eq!(b, Err(TlsError::Closed));
    assert_eq!(c, Err(TlsError::Closed));
}

#[test]
fn mutated_server_messages_never_panic() {
    // Edit one of the server's handshake messages at random (the server hashes and signs
    // what it sends, so only the parser and the checks stand in the way).
    let mut r = Prng(0xfa11_0f_f00d);
    let mut ok = 0;
    for i in 0..400 {
        let target = [2u8, 8, 11, 15, 20][i % 5];
        let mut local = Prng(r.next());
        let edit: Edit = Box::new(move |kind, m| {
            if kind == target {
                // Mutate the body and keep the header's length right (a wrong length just
                // makes both sides wait for bytes that never come).
                let mut body = m[4..].to_vec();
                mutate(&mut body, &mut local);
                let mut fixed = hs(m[0], &body);
                core::mem::swap(m, &mut fixed);
            }
        });
        let opts = Opts {
            edit: Some(edit),
            echo: false,
            ..Opts::default()
        };
        let (res, _) = run(config(&["root-ed"]), HOST, opts, |_| ());
        if res.is_ok() {
            ok += 1;
            // Only an edit the client may legitimately not care about is accepted (the
            // server's random in ServerHello, an extension's contents in
            // EncryptedExtensions); a changed certificate, signature or Finished never is.
            assert!(target == 2 || target == 8, "a mutated message {target} was accepted");
        }
    }
    assert!(ok < 400);
}
