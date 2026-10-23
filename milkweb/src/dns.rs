//! DNS for a stub resolver (RFC 1035, RFC 3596 for AAAA): queries, answers, a TTL cache and
//! the lookup logic, with the sending left to a [`Host`].
//!
//! - [`query`] builds a question (recursion desired); [`parse`] reads a whole message:
//!   header, the question, answers (A, AAAA, CNAME; others kept as `Other`), and an SOA in
//!   the authority section for the negative-answer TTL. Names follow compression pointers,
//!   but only backwards and at most 64 of them, so a hostile message can't loop.
//! - [`lookup`] asks for one type: the cache first, then UDP; a truncated (TC) answer is
//!   asked again over TCP. A CNAME chain is followed inside the answer and, if it ends
//!   without addresses, with new questions for its target (at most [`MAX_CNAMES`]).
//!   Results, NXDOMAIN and "no such record" are cached for their TTL (clamped).
//! - [`resolve`]: A and AAAA together (what `network lookup` and the HTTP client use).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

pub const TYPE_A: u16 = 1;
pub const TYPE_NS: u16 = 2;
pub const TYPE_CNAME: u16 = 5;
pub const TYPE_SOA: u16 = 6;
pub const TYPE_AAAA: u16 = 28;
pub const CLASS_IN: u16 = 1;
/// CNAMEs followed for one lookup.
pub const MAX_CNAMES: usize = 8;
/// TTLs are kept between these (seconds).
pub const MIN_TTL: u32 = 5;
pub const MAX_TTL: u32 = 86_400;
/// For NXDOMAIN / no data without an SOA (seconds), and the most a negative answer is kept.
pub const NEG_TTL: u32 = 60;
pub const MAX_NEG_TTL: u32 = 900;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DnsError {
    /// Not a name we can ask about (empty label, too long, odd characters).
    BadName,
    /// NXDOMAIN: there is no such name.
    NotFound,
    /// The name exists but has no record of that type.
    NoData,
    /// No answer in time.
    Timeout,
    /// The server answered with this RCODE (2 SERVFAIL, 5 REFUSED, ...).
    Server(u8),
    /// The answer couldn't be read (what was wrong).
    Malformed(&'static str),
    /// Sending failed (the host says why).
    Io(String),
    /// The CNAME chain was longer than [`MAX_CNAMES`] or went round in a circle.
    CnameLoop,
}

impl fmt::Display for DnsError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DnsError::BadName => f.write_str("not a valid host name"),
            DnsError::NotFound => f.write_str("there's no such host"),
            DnsError::NoData => f.write_str("the host has no address of that kind"),
            DnsError::Timeout => f.write_str("the name server didn't answer"),
            DnsError::Server(2) => f.write_str("the name server failed (SERVFAIL)"),
            DnsError::Server(5) => f.write_str("the name server refused the question"),
            DnsError::Server(r) => write!(f, "the name server answered with error {r}"),
            DnsError::Malformed(m) => write!(f, "a bad answer from the name server: {m}"),
            DnsError::Io(m) => f.write_str(m),
            DnsError::CnameLoop => f.write_str("the name's aliases (CNAMEs) go round in a circle"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Addr {
    V4([u8; 4]),
    V6([u8; 16]),
}

impl Addr {
    pub fn is_v6(&self) -> bool {
        matches!(self, Addr::V6(_))
    }
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Addr::V4(a) => write!(f, "{}.{}.{}.{}", a[0], a[1], a[2], a[3]),
            Addr::V6(a) => {
                // RFC 5952: the longest run of zero groups (2+) becomes ::.
                let g: Vec<u16> = (0..8)
                    .map(|i| u16::from_be_bytes([a[2 * i], a[2 * i + 1]]))
                    .collect();
                let (mut best, mut best_len, mut i) = (8, 0, 0);
                while i < 8 {
                    if g[i] == 0 {
                        let s = i;
                        while i < 8 && g[i] == 0 {
                            i += 1;
                        }
                        if i - s > best_len && i - s >= 2 {
                            best = s;
                            best_len = i - s;
                        }
                    } else {
                        i += 1;
                    }
                }
                let mut k = 0;
                let mut first = true;
                while k < 8 {
                    if k == best {
                        f.write_str("::")?;
                        k += best_len;
                        first = true;
                        continue;
                    }
                    if !first {
                        f.write_str(":")?;
                    }
                    write!(f, "{:x}", g[k])?;
                    first = false;
                    k += 1;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RData {
    A([u8; 4]),
    Aaaa([u8; 16]),
    Cname(String),
    Other,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Record {
    /// Lower case, no trailing dot.
    pub name: String,
    pub rtype: u16,
    pub ttl: u32,
    pub data: RData,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: u16,
    pub response: bool,
    pub truncated: bool,
    pub rcode: u8,
    /// The first question (name, type), if there was one.
    pub question: Option<(String, u16)>,
    pub answers: Vec<Record>,
    /// The SOA MINIMUM (bounded by the SOA's own TTL) from the authority section: how long a
    /// negative answer may be kept (RFC 2308).
    pub neg_ttl: Option<u32>,
}

/// Lower case, no trailing dot; checks it is a name we may put in a question.
pub fn normalize(name: &str) -> Result<String, DnsError> {
    let n = name.trim().trim_end_matches('.').to_ascii_lowercase();
    let ok = !n.is_empty()
        && n.len() <= 253
        && n.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        });
    if ok { Ok(n) } else { Err(DnsError::BadName) }
}

/// A query for `name` (already [`normalize`]d), type `qtype`, recursion desired.
pub fn query(id: u16, name: &str, qtype: u16) -> Result<Vec<u8>, DnsError> {
    let name = normalize(name)?;
    let mut q = Vec::with_capacity(18 + name.len());
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // one question
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(q)
}

fn be16(m: &[u8], k: usize) -> Result<u16, DnsError> {
    m.get(k..k + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or(DnsError::Malformed("cut off"))
}

fn be32(m: &[u8], k: usize) -> Result<u32, DnsError> {
    m.get(k..k + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(DnsError::Malformed("cut off"))
}

/// Read the name at `k` (following compression pointers): (name, offset after it in the
/// record, i.e. after the first pointer if there was one).
pub fn read_name(m: &[u8], mut k: usize) -> Result<(String, usize), DnsError> {
    let mut name = String::new();
    let mut end = None;
    let mut jumps = 0;
    loop {
        let len = *m
            .get(k)
            .ok_or(DnsError::Malformed("a name runs off the end"))? as usize;
        match len & 0xc0 {
            0 => {
                if len == 0 {
                    return Ok((name, end.unwrap_or(k + 1)));
                }
                let label = m
                    .get(k + 1..k + 1 + len)
                    .ok_or(DnsError::Malformed("a label runs off the end"))?;
                if !name.is_empty() {
                    name.push('.');
                }
                for &c in label {
                    name.push((c as char).to_ascii_lowercase());
                }
                if name.len() > 255 {
                    return Err(DnsError::Malformed("a name is too long"));
                }
                k += 1 + len;
            }
            0xc0 => {
                let target = (be16(m, k)? & 0x3fff) as usize;
                // Only backwards (to something already seen), and not forever.
                if target >= k || jumps >= 64 {
                    return Err(DnsError::Malformed("a compression pointer loops"));
                }
                jumps += 1;
                end.get_or_insert(k + 2);
                k = target;
            }
            _ => return Err(DnsError::Malformed("a reserved label type")),
        }
    }
}

/// Parse a whole message.
pub fn parse(m: &[u8]) -> Result<Message, DnsError> {
    if m.len() < 12 {
        return Err(DnsError::Malformed("shorter than a header"));
    }
    let id = be16(m, 0)?;
    let flags = be16(m, 2)?;
    let (qd, an, ns) = (be16(m, 4)?, be16(m, 6)?, be16(m, 8)?);
    let mut k = 12;
    let mut question = None;
    for i in 0..qd {
        let (name, after) = read_name(m, k)?;
        let qtype = be16(m, after)?;
        if i == 0 {
            question = Some((name, qtype));
        }
        k = after + 4;
    }
    let mut answers = Vec::new();
    let mut neg_ttl = None;
    for i in 0..(an as usize + ns as usize) {
        let (name, after) = read_name(m, k)?;
        let rtype = be16(m, after)?;
        let class = be16(m, after + 2)?;
        let ttl = be32(m, after + 4)?;
        let rdlen = be16(m, after + 8)? as usize;
        let rd = after + 10;
        let rdata = m
            .get(rd..rd + rdlen)
            .ok_or(DnsError::Malformed("a record runs off the end"))?;
        // A TTL with the top bit set is treated as 0 (RFC 2181 8).
        let ttl = if ttl & 0x8000_0000 != 0 { 0 } else { ttl };
        if i < an as usize {
            let data = match (rtype, class, rdlen) {
                (TYPE_A, CLASS_IN, 4) => RData::A([rdata[0], rdata[1], rdata[2], rdata[3]]),
                (TYPE_A, CLASS_IN, _) => {
                    return Err(DnsError::Malformed("an A record isn't 4 bytes"));
                }
                (TYPE_AAAA, CLASS_IN, 16) => {
                    let mut a = [0u8; 16];
                    a.copy_from_slice(rdata);
                    RData::Aaaa(a)
                }
                (TYPE_AAAA, CLASS_IN, _) => {
                    return Err(DnsError::Malformed("an AAAA record isn't 16 bytes"));
                }
                (TYPE_CNAME, CLASS_IN, _) => {
                    let (target, after) = read_name(m, rd)?;
                    if after > rd + rdlen {
                        return Err(DnsError::Malformed("a CNAME runs past its record"));
                    }
                    RData::Cname(target)
                }
                _ => RData::Other,
            };
            answers.push(Record {
                name,
                rtype,
                ttl,
                data,
            });
        } else if rtype == TYPE_SOA {
            // MNAME, RNAME, then serial refresh retry expire minimum.
            let (_, a) = read_name(m, rd)?;
            let (_, b) = read_name(m, a)?;
            let minimum = be32(m, b + 16)?;
            if b + 20 <= rd + rdlen {
                neg_ttl = Some(minimum.min(ttl));
            }
        }
        k = rd + rdlen;
    }
    Ok(Message {
        id,
        response: flags & 0x8000 != 0,
        truncated: flags & 0x0200 != 0,
        rcode: (flags & 0x000f) as u8,
        question,
        answers,
        neg_ttl,
    })
}

/// Is `m` the answer to our question (`id`, `name`, `qtype`)?
pub fn answers_question(m: &Message, id: u16, name: &str, qtype: u16) -> bool {
    m.response
        && m.id == id
        && m.question
            .as_ref()
            .is_some_and(|(n, t)| n == name && *t == qtype)
}

/// What an answer says about `name`/`qtype`: the addresses at the end of the CNAME chain
/// starting at `name` (with the smallest TTL along it), or where the chain stops.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Chain {
    pub addrs: Vec<Addr>,
    pub ttl: u32,
    /// The name the chain ended at (`name` itself if there was no CNAME).
    pub canonical: String,
    /// CNAMEs followed.
    pub hops: usize,
}

pub fn follow(m: &Message, name: &str, qtype: u16) -> Result<Chain, DnsError> {
    let mut cur = String::from(name);
    let mut ttl = u32::MAX;
    let mut hops = 0;
    loop {
        let addrs: Vec<(Addr, u32)> = m
            .answers
            .iter()
            .filter(|r| r.name == cur && r.rtype == qtype)
            .filter_map(|r| match r.data {
                RData::A(a) => Some((Addr::V4(a), r.ttl)),
                RData::Aaaa(a) => Some((Addr::V6(a), r.ttl)),
                _ => None,
            })
            .collect();
        if !addrs.is_empty() {
            let t = addrs.iter().map(|(_, t)| *t).min().unwrap_or(0);
            return Ok(Chain {
                addrs: addrs.into_iter().map(|(a, _)| a).collect(),
                ttl: ttl.min(t),
                canonical: cur,
                hops,
            });
        }
        let next = m.answers.iter().find_map(|r| match &r.data {
            RData::Cname(t) if r.name == cur => Some((t.clone(), r.ttl)),
            _ => None,
        });
        match next {
            Some((t, rt)) => {
                hops += 1;
                if hops > MAX_CNAMES || t == name {
                    return Err(DnsError::CnameLoop);
                }
                ttl = ttl.min(rt);
                cur = t;
            }
            None => {
                return Ok(Chain {
                    addrs: Vec::new(),
                    ttl: if ttl == u32::MAX { 0 } else { ttl },
                    canonical: cur,
                    hops,
                });
            }
        }
    }
}

// ---- the cache --------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cached {
    Addrs(Vec<Addr>),
    NotFound,
    NoData,
}

#[derive(Clone, Debug)]
struct Entry {
    name: String,
    qtype: u16,
    expires: u64,
    value: Cached,
}

/// Answers by (name, type) until their TTL runs out; the oldest go first when it is full.
#[derive(Clone, Debug)]
pub struct Cache {
    entries: Vec<Entry>,
    cap: usize,
    pub hits: u64,
    pub misses: u64,
}

impl Cache {
    pub const fn new(cap: usize) -> Cache {
        Cache {
            entries: Vec::new(),
            cap,
            hits: 0,
            misses: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The live answer for `name`/`qtype` at time `now` (seconds), if any.
    pub fn get(&mut self, name: &str, qtype: u16, now: u64) -> Option<Cached> {
        self.entries.retain(|e| e.expires > now);
        let r = self
            .entries
            .iter()
            .find(|e| e.qtype == qtype && e.name == name)
            .map(|e| e.value.clone());
        if r.is_some() {
            self.hits += 1;
        } else {
            self.misses += 1;
        }
        r
    }

    /// Keep `value` for `ttl` seconds (clamped: positive to [MIN_TTL, MAX_TTL], negative to
    /// [MIN_TTL, MAX_NEG_TTL]).
    pub fn put(&mut self, name: &str, qtype: u16, value: Cached, ttl: u32, now: u64) {
        let ttl = match value {
            Cached::Addrs(_) => ttl.clamp(MIN_TTL, MAX_TTL),
            _ => ttl.clamp(MIN_TTL, MAX_NEG_TTL),
        };
        self.entries
            .retain(|e| !(e.qtype == qtype && e.name == name) && e.expires > now);
        if self.cap == 0 {
            return;
        }
        while self.entries.len() >= self.cap {
            // The one closest to expiring.
            let (i, _) = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.expires)
                .unwrap();
            self.entries.remove(i);
        }
        self.entries.push(Entry {
            name: String::from(name),
            qtype,
            expires: now + ttl as u64,
            value,
        });
    }

    /// (name, type, seconds left, value) of every live entry.
    pub fn list(&self, now: u64) -> Vec<(String, u16, u64, Cached)> {
        self.entries
            .iter()
            .filter(|e| e.expires > now)
            .map(|e| (e.name.clone(), e.qtype, e.expires - now, e.value.clone()))
            .collect()
    }
}

// ---- lookups ----------------------------------------------------------------------------------

/// What [`lookup`] needs: sending, a clock, ids and the cache.
pub trait Host {
    /// Send `query` over UDP to the server and return the first datagram `ok` accepts
    /// (retrying as it sees fit); [`DnsError::Timeout`] if none comes.
    fn udp(&mut self, query: &[u8], ok: &mut dyn FnMut(&[u8]) -> bool)
    -> Result<Vec<u8>, DnsError>;
    /// The same over TCP (the host adds and strips the two-byte length).
    fn tcp(&mut self, query: &[u8]) -> Result<Vec<u8>, DnsError>;
    /// Seconds on a monotonic clock.
    fn now_secs(&mut self) -> u64;
    /// A fresh query id (random if possible).
    fn new_id(&mut self) -> u16;
    fn cache_get(&mut self, name: &str, qtype: u16, now: u64) -> Option<Cached>;
    fn cache_put(&mut self, name: &str, qtype: u16, value: Cached, ttl: u32, now: u64);
}

/// Ask once (UDP, then TCP if truncated) and return the parsed answer.
fn ask(h: &mut dyn Host, name: &str, qtype: u16) -> Result<Message, DnsError> {
    let id = h.new_id();
    let q = query(id, name, qtype)?;
    let mut ok = |b: &[u8]| parse(b).is_ok_and(|m| answers_question(&m, id, name, qtype));
    let raw = h.udp(&q, &mut ok)?;
    let m = parse(&raw)?;
    if !m.truncated {
        return Ok(m);
    }
    let id = h.new_id();
    let q = query(id, name, qtype)?;
    let raw = h.tcp(&q)?;
    let m = parse(&raw)?;
    if !answers_question(&m, id, name, qtype) {
        return Err(DnsError::Malformed("the TCP answer is to another question"));
    }
    Ok(m)
}

/// The `qtype` (A or AAAA) addresses of `name`.
pub fn lookup(h: &mut dyn Host, name: &str, qtype: u16) -> Result<Vec<Addr>, DnsError> {
    let original = normalize(name)?;
    let mut cur = original.clone();
    let mut visited: Vec<String> = Vec::new();
    for _ in 0..=MAX_CNAMES {
        let now = h.now_secs();
        match h.cache_get(&cur, qtype, now) {
            Some(Cached::Addrs(a)) => {
                if cur != original {
                    h.cache_put(&original, qtype, Cached::Addrs(a.clone()), MIN_TTL, now);
                }
                return Ok(a);
            }
            Some(Cached::NotFound) => return Err(DnsError::NotFound),
            Some(Cached::NoData) => return Err(DnsError::NoData),
            None => {}
        }
        let m = ask(h, &cur, qtype)?;
        let now = h.now_secs();
        match m.rcode {
            0 => {}
            3 => {
                let ttl = m.neg_ttl.unwrap_or(NEG_TTL);
                h.cache_put(&cur, qtype, Cached::NotFound, ttl, now);
                if cur != original {
                    h.cache_put(&original, qtype, Cached::NotFound, ttl, now);
                }
                return Err(DnsError::NotFound);
            }
            r => return Err(DnsError::Server(r)),
        }
        let c = follow(&m, &cur, qtype)?;
        if !c.addrs.is_empty() {
            h.cache_put(&cur, qtype, Cached::Addrs(c.addrs.clone()), c.ttl, now);
            if c.canonical != cur {
                h.cache_put(
                    &c.canonical,
                    qtype,
                    Cached::Addrs(c.addrs.clone()),
                    c.ttl,
                    now,
                );
            }
            if cur != original {
                h.cache_put(&original, qtype, Cached::Addrs(c.addrs.clone()), c.ttl, now);
            }
            return Ok(c.addrs);
        }
        if c.hops == 0 {
            let ttl = m.neg_ttl.unwrap_or(NEG_TTL);
            h.cache_put(&cur, qtype, Cached::NoData, ttl, now);
            if cur != original {
                h.cache_put(&original, qtype, Cached::NoData, ttl, now);
            }
            return Err(DnsError::NoData);
        }
        // The chain ended at a name the answer has no addresses for: ask about that.
        visited.push(cur.clone());
        if visited.contains(&c.canonical) || c.canonical == original {
            return Err(DnsError::CnameLoop);
        }
        cur = c.canonical;
    }
    Err(DnsError::CnameLoop)
}

/// Which address families to look up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    Any,
    V4,
    V6,
}

/// A and / or AAAA for `name`: IPv4 first (slirp's IPv6 only works if the Mac has it).
/// An error only if every lookup failed (the first error then, NotFound before NoData).
pub fn resolve(h: &mut dyn Host, name: &str, want: Want) -> Result<Vec<Addr>, DnsError> {
    let mut out = Vec::new();
    let mut errs = Vec::new();
    for (qtype, on) in [(TYPE_A, want != Want::V6), (TYPE_AAAA, want != Want::V4)] {
        if !on {
            continue;
        }
        match lookup(h, name, qtype) {
            Ok(a) => out.extend(a),
            Err(DnsError::NotFound) => return Err(DnsError::NotFound),
            Err(e) => errs.push(e),
        }
    }
    if !out.is_empty() {
        return Ok(out);
    }
    Err(errs.into_iter().next().unwrap_or(DnsError::NoData))
}

/// The type's name for messages (`A`, `AAAA`, `CNAME`, or the number).
pub fn type_name(t: u16) -> String {
    match t {
        TYPE_A => String::from("A"),
        TYPE_AAAA => String::from("AAAA"),
        TYPE_CNAME => String::from("CNAME"),
        TYPE_NS => String::from("NS"),
        TYPE_SOA => String::from("SOA"),
        t => format!("TYPE{t}"),
    }
}
