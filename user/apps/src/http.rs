//! Minimal HTTP/1.0 GET over the socket syscalls. Repository URLs must name an IP address
//! or `localhost` (no DNS: milkOS never resolves names for user programs, and the repo is
//! a server on the Mac's 127.0.0.1, seen as 10.0.2.2).

use alloc::string::String;
use alloc::vec::Vec;
use rt::net::{self, SockAddr};
use rt::sys;

pub struct Url {
    pub addr: SockAddr,
    pub host: String,
    /// Always ends with '/'.
    pub base_path: String,
}

/// `http://IP[:PORT][/path/]`.
pub fn parse_url(u: &str) -> Result<Url, String> {
    let rest = u
        .strip_prefix("http://")
        .ok_or_else(|| alloc::format!("{u}: only http:// repositories"))?;
    let (hostport, path) = match rest.find('/') {
        Some(k) => (&rest[..k], &rest[k..]),
        None => (rest, "/"),
    };
    let host = hostport.rsplit_once(':').map_or(hostport, |(h, _)| h);
    let is_ip = host.split('.').count() == 4 && host.split('.').all(|p| p.parse::<u8>().is_ok());
    if !is_ip && host != "localhost" {
        return Err(alloc::format!(
            "{u}: repository host must be an IP address or localhost (no name lookups)"
        ));
    }
    let addr = SockAddr::parse(hostport, 80).ok_or_else(|| alloc::format!("{u}: bad address"))?;
    let mut base_path = String::from(path);
    if !base_path.ends_with('/') {
        base_path.push('/');
    }
    Ok(Url {
        addr,
        host: String::from(hostport),
        base_path,
    })
}

/// GET `file` relative to the repository. Returns the body of a 200 response.
pub fn get(repo: &Url, file: &str) -> Result<Vec<u8>, String> {
    let fd = net::socket(net::SOCK_STREAM).map_err(|e| alloc::format!("socket: {e}"))?;
    let r = (|| {
        let _ = net::set_recv_timeout(fd, 10_000);
        net::connect(fd, &repo.addr).map_err(|e| alloc::format!("connect {}: {e}", repo.addr))?;
        let req = alloc::format!(
            "GET {}{} HTTP/1.0\r\nHost: {}\r\nUser-Agent: milkOS-apps/1\r\nConnection: close\r\n\r\n",
            repo.base_path,
            file,
            repo.host
        );
        let mut b = req.as_bytes();
        while !b.is_empty() {
            let n = net::send(fd, b).map_err(|e| alloc::format!("send: {e}"))?;
            b = &b[n..];
        }
        let mut resp = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match net::recv(fd, &mut buf) {
                Ok(0) => break,
                Ok(n) => resp.extend_from_slice(&buf[..n]),
                Err(e) if e.0 == 11 => return Err(String::from("timed out")),
                Err(e) => return Err(alloc::format!("recv: {e}")),
            }
            if resp.len() > 64 << 20 {
                return Err(String::from("response too big"));
            }
        }
        let split = resp
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .ok_or("bad HTTP response")?;
        let head = String::from_utf8_lossy(&resp[..split]).into_owned();
        let status = head.lines().next().unwrap_or("");
        if status.split(' ').nth(1) != Some("200") {
            return Err(alloc::format!("{file}: {}", status.trim()));
        }
        let body = resp[split + 4..].to_vec();
        let len = head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<usize>().ok())?
        });
        if len.is_some_and(|n| n != body.len()) {
            return Err(alloc::format!(
                "{file}: short body ({} of {} bytes)",
                body.len(),
                len.unwrap()
            ));
        }
        Ok(body)
    })();
    let _ = sys::close(fd);
    r
}
