//! Sizes, counts and times that keep a page's requests from eating the machine, and the
//! [`Limiter`] that admits, queues and refuses requests for one tab.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use super::error::NetError;

/// Hard limits (one set for the whole Browser; the bindings may use smaller ones).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The address text, for http(s) and relative addresses.
    pub max_url: usize,
    /// Request headers a script sets: how many, and name+value bytes in all.
    pub max_headers: usize,
    pub max_headers_bytes: usize,
    pub max_header_value: usize,
    /// A request body.
    pub max_request_body: usize,
    /// A `keepalive` request's body (the web's 64 KiB).
    pub max_keepalive_body: usize,
    /// A response body, as the page may read it (also the `data:` URL's decoded size).
    pub max_response_body: usize,
    /// Status line plus headers of a response.
    pub max_response_head: usize,
    /// Redirects followed (the Fetch standard's 20).
    pub max_redirects: usize,
    /// The whole request, redirects and a preflight included.
    pub timeout_ms: u64,
    /// In flight at once for a tab, and for one host:port. Beyond the per-host number a
    /// request waits in line; beyond `max_pending` (running plus waiting) it is refused.
    pub max_per_tab: usize,
    pub max_per_host: usize,
    pub max_pending: usize,
    /// The longest a preflight's answer is remembered (seconds; the web's default is 5, and
    /// browsers cap what a server asks for).
    pub preflight_default_age: u64,
    pub preflight_max_age: u64,
    pub preflight_cache_entries: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            max_url: 8192,
            max_headers: 64,
            max_headers_bytes: 16 * 1024,
            max_header_value: 8192,
            max_request_body: 4 << 20,
            max_keepalive_body: 64 * 1024,
            max_response_body: 8 << 20,
            max_response_head: 32 * 1024,
            max_redirects: 20,
            timeout_ms: 30_000,
            max_per_tab: 16,
            max_per_host: 6,
            max_pending: 64,
            preflight_default_age: 5,
            preflight_max_age: 600,
            preflight_cache_entries: 128,
        }
    }
}

/// The Browser-wide settings for script networking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub limits: Limits,
    /// Refuse a request from a page on a public host to a private-network, loopback or local
    /// address (an IP literal or a `.localhost` / `.local` / one-label name). On by default;
    /// a test setup whose pages and servers are all on 10.0.2.x turns it off.
    pub block_private_network: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            limits: Limits::default(),
            block_private_network: true,
        }
    }
}

impl Config {
    /// The timeout to use: the one asked for (0 or none: the default), never above the
    /// limit.
    pub fn effective_timeout(&self, asked: Option<u64>) -> u64 {
        match asked {
            Some(ms) if ms > 0 => ms.min(self.limits.timeout_ms),
            _ => self.limits.timeout_ms,
        }
    }
}

/// What to do with a request that wants to start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admission {
    Start,
    /// Wait: [`Limiter::finish`] will say when.
    Queued,
}

/// Counts one tab's requests. Ids are the bindings' own (any distinct numbers); a request's
/// host is `host:port`.
#[derive(Clone, Debug)]
pub struct Limiter {
    max_per_tab: usize,
    max_per_host: usize,
    max_pending: usize,
    running: Vec<(u64, String)>,
    waiting: VecDeque<(u64, String)>,
}

impl Limiter {
    pub fn new(limits: &Limits) -> Limiter {
        Limiter {
            max_per_tab: limits.max_per_tab.max(1),
            max_per_host: limits.max_per_host.max(1),
            max_pending: limits.max_pending.max(1),
            running: Vec::new(),
            waiting: VecDeque::new(),
        }
    }

    pub fn running(&self) -> usize {
        self.running.len()
    }

    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// Anything outstanding (for the tab's loading indicator).
    pub fn busy(&self) -> bool {
        !self.running.is_empty() || !self.waiting.is_empty()
    }

    fn host_count(&self, host: &str) -> usize {
        self.running.iter().filter(|(_, h)| h == host).count()
    }

    fn can_run(&self, host: &str) -> bool {
        self.running.len() < self.max_per_tab && self.host_count(host) < self.max_per_host
    }

    /// A request wants to start. A reused id counts as a mistake and is refused.
    pub fn submit(&mut self, id: u64, host: &str) -> Result<Admission, NetError> {
        if self.running.iter().any(|(i, _)| *i == id) || self.waiting.iter().any(|(i, _)| *i == id)
        {
            return Err(NetError::OutOfOrder("request id used twice"));
        }
        if self.running.len() + self.waiting.len() >= self.max_pending {
            return Err(NetError::TooManyRequests {
                max: self.max_pending,
            });
        }
        // Same-host requests keep their order; one for a free host doesn't wait behind others.
        if self.can_run(host) && !self.waiting.iter().any(|(_, h)| h == host) {
            self.running.push((id, String::from(host)));
            Ok(Admission::Start)
        } else {
            self.waiting.push_back((id, String::from(host)));
            Ok(Admission::Queued)
        }
    }

    /// A request is over (done, failed, aborted, or cancelled while waiting). Returns the
    /// waiting requests that may start now, in order.
    pub fn finish(&mut self, id: u64) -> Vec<u64> {
        self.running.retain(|(i, _)| *i != id);
        self.waiting.retain(|(i, _)| *i != id);
        self.promote()
    }

    /// Everything over at once (the tab navigated or closed). Returns nothing to start.
    pub fn clear(&mut self) {
        self.running.clear();
        self.waiting.clear();
    }

    fn promote(&mut self) -> Vec<u64> {
        let mut started = Vec::new();
        // The first waiting request that can run, repeatedly (a request blocked by its own
        // host doesn't hold back one for another host).
        while let Some(pos) = self.waiting.iter().position(|(_, h)| self.can_run(h)) {
            let (id, host) = self.waiting.remove(pos).unwrap_or((0, String::new()));
            self.running.push((id, host));
            started.push(id);
        }
        started
    }
}
