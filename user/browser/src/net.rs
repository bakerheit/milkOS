//! Fetching, off the window's thread: every page, image and download is a job on its own
//! thread (rt::thread, a 512 KiB stack for the TLS handshake) running milkweb's
//! `http::fetch` over rt's sockets, with the Browser's hooks: the cookie jar asked and fed on
//! every hop, progress and Stop, and a page that turns out to be a file becomes a download
//! (saved to ~/Downloads by the job itself). The window polls each job's [`Shared`] state.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use browserkit::certinfo::{self, CertInfo};
use browserkit::cookies::{Context, Jar};
use browserkit::download;
use browserkit::netpolicy::{Config as NetConfig, Page as NetPage, PreflightCache};
use imgdec::Image;
use milkweb::http::{
    self, Error, Failure, Hooks, Method, Net, Options, Request, Response, Transport,
};
use milkweb::url::Url;
use rt::net::Want;
use rt::sys;
use rt::thread::{self, JoinHandle, Mutex};
use rt::web::RtNet;
use webengine::{NetHead, NetRequest};

pub type JobId = u64;

/// What the fetch limits were before they followed memory (`milkweb` without a plan, view-source,
/// frames, which are fetched whole): the Browser's `Caps` replace them for pages, scripts,
/// stylesheets and pictures (`Fetcher::caps`).
pub const MAX_PAGE: usize = 4 << 20;
pub const MAX_STYLE: usize = 2 << 20;
pub const MAX_SCRIPT: usize = 4 << 20;
pub const MAX_IMAGE: usize = 8 << 20;
/// A page that streams in is held back from the window's thread past this many bytes (the
/// job waits: backpressure).
const STREAM_WINDOW: usize = 2 << 20;
/// Jobs running at once (more wait their turn).
pub const MAX_RUNNING: usize = 6;
/// Script requests (fetch, XMLHttpRequest) running at once, besides those: a long poll or two
/// must not keep a page from loading.
pub const MAX_RUNNING_REQUESTS: usize = 8;
const STACK: usize = 512 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A page for a tab (it may turn out to be a download).
    Page,
    Image,
    /// A stylesheet for a page.
    Style,
    /// An external script for a page.
    Script,
    /// Save whatever comes (Alt-click on a link).
    Download,
    /// A request a page's script made (fetch, XMLHttpRequest): run by the network policy.
    Request,
    /// A document for an `<iframe>`: fetched like a page, but never a download, and every hop is
    /// checked against the frame rules (mixed content, refused ports).
    Frame,
}

/// A decoded picture: a still one, or an animation (GIF, WebP) to step through.
pub enum Picture {
    Still(Image),
    Moving(imgdec::anim::Animation),
}

pub enum Outcome {
    Page(Response),
    Image(Result<Picture, String>),
    /// A download was written: its path and size.
    Saved(String, usize),
    Failed(Failure),
    /// A download came, but couldn't be written (why).
    SaveFailed(String),
    /// A script request's end.
    Net(Box<Result<scriptnet::Done, scriptnet::Failed>>),
    /// A frame's fetch was stopped by a rule on the way (a redirect to plain http under a secure
    /// page, ...).
    Refused(scriptnet::frame::load::Refusal),
}

/// The head of a page that streams in.
#[derive(Clone, Debug)]
pub struct PageHead {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub url: Url,
}

/// A page's body on its way from the job to the window's thread.
#[derive(Default)]
pub struct StreamBuf {
    pub chunks: VecDeque<Vec<u8>>,
    /// Bytes waiting in `chunks`.
    pub waiting: usize,
    /// Bytes handed over in all.
    pub total: usize,
    /// The job stopped reading at the cap: the page is longer than this.
    pub cut: bool,
}

/// What a job and the window share.
#[derive(Default)]
pub struct Shared {
    /// A page that streams in: its head (set once, when the final answer's head is in) and the
    /// body so far.
    pub page_head: Option<PageHead>,
    pub stream: StreamBuf,
    /// A script request that streams: how many bytes of its body the page has used (the job
    /// sends no more than a window ahead of it).
    pub acked: u64,
    pub got: usize,
    pub total: Option<usize>,
    /// The window asks the job to stop.
    pub cancel: bool,
    /// The response is a file to save, under this name.
    pub download: Option<String>,
    /// The secure connection's details (https).
    pub cert: Option<CertInfo>,
    pub done: Option<Outcome>,
    /// A script request: the response's head, once known (the window takes it).
    pub head: Option<NetHead>,
    /// A page: the address it was fetched from (4 or 16 bytes), for the private-network rule.
    pub peer: Option<Vec<u8>>,
    /// A frame's fetch hit a rule on a redirect: why it was stopped.
    pub refused: Option<scriptnet::frame::load::Refusal>,
}

pub struct Job {
    pub id: JobId,
    pub kind: Kind,
    pub shared: Arc<Mutex<Shared>>,
    handle: Option<JoinHandle>,
    spec: Option<Spec>,
}

struct Spec {
    req: Request,
    ctx: Context,
    /// A page: its body streams to the window's thread (up to this many bytes) instead of
    /// coming whole.
    stream_cap: Option<usize>,
    /// The most an image, a stylesheet, a script and a page that comes whole (view-source, a
    /// frame's document) may be (from the memory plan).
    caps: (usize, usize, usize, usize),
    /// A script request, and what the policy needs to run it.
    script: Option<ScriptSpec>,
    /// A frame: the addresses of its ancestors (top first), checked on every hop.
    chain: Option<Vec<Option<Url>>>,
}

/// A script request to run.
pub struct ScriptSpec {
    pub req: NetRequest,
    pub page: NetPage,
    pub cfg: NetConfig,
    pub cache: Arc<Mutex<PreflightCache>>,
}

/// milkweb's `Net` over rt's sockets, with a TLS client made once per job and the
/// connection details kept for the padlock.
struct BrowserNet {
    inner: RtNet,
    client: Option<tls::Client>,
    shared: Arc<Mutex<Shared>>,
}

impl Net for BrowserNet {
    fn connect(
        &mut self,
        host: &str,
        port: u16,
        timeout_ms: u64,
    ) -> Result<Box<dyn Transport>, Error> {
        self.inner.connect(host, port, timeout_ms)
    }
    fn now_ms(&mut self) -> u64 {
        self.inner.now_ms()
    }
    fn has_tls(&self) -> bool {
        true
    }
    fn secure(&mut self, host: &str, tcp: Box<dyn Transport>) -> Result<Box<dyn Transport>, Error> {
        let client = self.client.get_or_insert_with(rt::tls::client);
        let (t, info) = milkweb::secure::secure_info(client, host, tcp)?;
        self.shared.lock().cert = Some(certinfo::describe(&info));
        Ok(t)
    }
}

impl scriptnet::ScriptNet for BrowserNet {
    /// The same lookups as `rt::web` (so names, `localhost`, `.local` and numeric addresses
    /// behave as for pages), but the addresses come back to the policy, which refuses the
    /// ones a public page may not reach before anything is connected.
    fn resolve(&mut self, host: &str) -> Result<Vec<Vec<u8>>, Error> {
        use rt::net::{Ip, SockAddr};
        let want = self.inner.want;
        let local = host == "localhost" || SockAddr::parse(host, 0).is_some() || rt::mdns::is_local(host);
        let addrs: Vec<SockAddr> = if local {
            rt::net::resolve_all(host, want).map_err(Error::NoDns)?
        } else {
            let ips = rt::net::kernel_resolve(host, want).map_err(|e| match e.0 {
                1 => Error::Offline(String::from(host)),
                2 | 61 => Error::NotFound(String::from(host)),
                22 => Error::Url(milkweb::url::UrlError::BadHost(String::from(host))),
                _ => Error::NoDns(rt::net::lookup_words(host, e)),
            })?;
            ips.into_iter().map(|ip| SockAddr::new(ip, 0)).collect()
        };
        Ok(addrs
            .into_iter()
            .map(|a| match a.ip.unmapped() {
                Ip::V4(b) => b.to_vec(),
                Ip::V6(b) => b.to_vec(),
            })
            .collect())
    }

    fn connect_ip(&mut self, ip: &[u8], port: u16, timeout_ms: u64) -> Result<Box<dyn Transport>, Error> {
        use rt::net::Ip;
        let text = match ip.len() {
            4 => alloc::format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
            16 => {
                let mut b = [0u8; 16];
                b.copy_from_slice(ip);
                alloc::format!("{}", Ip::V6(b))
            }
            _ => return Err(Error::Connect(String::from("a bad address"))),
        };
        // A numeric address connects without another lookup.
        self.inner.connect(&text, port, timeout_ms)
    }
}

pub fn now_secs() -> i64 {
    (sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000) as i64
}

/// A download being written to disk as it comes.
struct DlFile {
    fd: i32,
    path: String,
    n: usize,
    err: Option<String>,
}

struct BrowserHooks {
    kind: Kind,
    downloads: String,
    file: Option<DlFile>,
    stream_cap: Option<usize>,
    reason: String,
    caps: (usize, usize, usize, usize),
    jar: Arc<Mutex<Jar>>,
    ctx: Context,
    shared: Arc<Mutex<Shared>>,
    chain: Option<Vec<Option<Url>>>,
    /// The address being fetched (for resolving a redirect's Location).
    base: Url,
}

fn is_redirect(status: u16, headers: &[(String, String)]) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308) && headers.iter().any(|(k, _)| k == "location")
}

impl Hooks for BrowserHooks {
    fn response_head(
        &mut self,
        url: &Url,
        status: u16,
        reason: &str,
        headers: &[(String, String)],
    ) -> Option<usize> {
        self.reason = String::from(reason);
        self.response(url, status, headers)
    }

    fn stream(&mut self, url: &Url, status: u16, headers: &[(String, String)]) -> bool {
        // A download goes to disk as it comes, whatever its size (the disk is the limit).
        let name = self.shared.lock().download.clone();
        if let Some(name) = name
            && (status < 400 || self.kind == Kind::Download)
        {
            match open_download(&self.downloads, &name) {
                Ok((fd, path)) => self.file = Some(DlFile { fd, path, n: 0, err: None }),
                Err(e) => {
                    // (No file: the body is read and thrown away, the outcome says why.)
                    self.file = Some(DlFile { fd: -1, path: String::new(), n: 0, err: Some(e) });
                }
            }
            return true;
        }
        if self.stream_cap.is_none() || self.kind != Kind::Page || name_is_some(&self.shared) {
            return false;
        }
        self.shared.lock().page_head = Some(PageHead {
            status,
            reason: self.reason.clone(),
            headers: headers.to_vec(),
            url: url.clone(),
        });
        true
    }

    fn body(&mut self, data: &[u8]) -> bool {
        if let Some(f) = self.file.as_mut() {
            if f.err.is_some() {
                return false;
            }
            if let Err(e) = sys::write_all(f.fd, data) {
                f.err = Some(alloc::format!("couldn't write it: {}", e.name()));
                return false;
            }
            f.n += data.len();
            return !self.shared.lock().cancel;
        }
        let cap = self.stream_cap.unwrap_or(usize::MAX);
        loop {
            {
                let mut s = self.shared.lock();
                if s.cancel {
                    return false;
                }
                // The window's thread takes what is waiting every few milliseconds; while a
                // lot is, the connection waits (and so does the server).
                if s.stream.waiting < STREAM_WINDOW {
                    s.stream.waiting += data.len();
                    s.stream.total += data.len();
                    s.stream.chunks.push_back(data.to_vec());
                    if s.stream.total >= cap {
                        s.stream.cut = true;
                        return false;
                    }
                    return true;
                }
            }
            sys::sleep_ms(3);
        }
    }

    fn cookie(&mut self, url: &Url, method: Method) -> Option<String> {
        self.jar
            .lock()
            .header(url, method == Method::Post, now_secs(), &self.ctx)
    }

    fn response(&mut self, url: &Url, status: u16, headers: &[(String, String)]) -> Option<usize> {
        self.jar.lock().set_all(url, headers, now_secs(), &self.ctx);
        self.base = url.clone();
        if is_redirect(status, headers) {
            // A frame's redirect is judged before it is followed: mixed content, refused ports.
            if let Some(chain) = &self.chain
                && let Some((_, loc)) = headers.iter().find(|(k, _)| k == "location")
                && let Ok(next) = url.join(loc)
                && let Err(why) = scriptnet::frame::load::check_hop(chain, &next)
            {
                self.shared.lock().refused = Some(why);
            }
            return None;
        }
        let h = |n: &str| {
            headers
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_str())
        };
        match self.kind {
            Kind::Image => Some(self.caps.0),
            Kind::Style | Kind::Script | Kind::Request | Kind::Frame => None,
            Kind::Page
                if !download::is_download(
                    h("content-type").unwrap_or(""),
                    h("content-disposition"),
                ) =>
            {
                None
            }
            Kind::Page | Kind::Download => {
                self.shared.lock().download =
                    Some(download::file_name(url, h("content-disposition")));
                // (The body goes to disk as it comes: no limit of memory's.)
                Some(usize::MAX / 4)
            }
        }
    }

    fn progress(&mut self, got: usize, total: Option<usize>) {
        let mut s = self.shared.lock();
        s.got = got;
        s.total = total;
    }

    fn stop(&mut self) -> bool {
        let s = self.shared.lock();
        s.cancel || s.refused.is_some()
    }
}

fn options(kind: Kind, caps: (usize, usize, usize, usize)) -> Options {
    Options {
        max_body: match kind {
            Kind::Image => caps.0,
            Kind::Style => caps.1,
            Kind::Script => caps.2,
            Kind::Frame => (caps.3 / 4).max(MAX_PAGE),
            _ => caps.3,
        },
        timeout_ms: if kind == Kind::Download {
            600_000
        } else {
            60_000
        },
        connect_timeout_ms: 10_000,
        accept: String::from(match kind {
            Kind::Image => "image/png, image/*;q=0.8, */*;q=0.5",
            Kind::Style => "text/css, */*;q=0.1",
            Kind::Script => "*/*",
            _ => "text/html, application/xhtml+xml, text/plain;q=0.9, */*;q=0.8",
        }),
        // Downloads come as they are (the file is the file).
        decompress: kind != Kind::Download,
        ..Options::default()
    }
}

/// Close a download's file and remove it.
fn discard_download(f: DlFile) {
    if f.fd >= 0 {
        let _ = sys::close(f.fd);
        let _ = sys::unlink(&f.path);
    }
}

fn name_is_some(sh: &Arc<Mutex<Shared>>) -> bool {
    sh.lock().download.is_some()
}

/// Make the file a download is written to, in `dir` under a free name: its descriptor and path.
fn open_download(dir: &str, name: &str) -> Result<(i32, String), String> {
    let _ = sys::mkdir(dir, 0o755);
    for _ in 0..3 {
        let n = download::unique(name, |n| {
            sys::stat_path(&alloc::format!("{dir}/{n}"), false).is_ok()
        });
        let path = alloc::format!("{dir}/{n}");
        match sys::open_mode(
            &path,
            sys::O_WRONLY | sys::O_CREAT | sys::O_EXCL | sys::O_CLOEXEC,
            0o644,
        ) {
            Ok(fd) => return Ok((fd, path)),
            Err(e) if e.0 == 17 => continue, // EEXIST: someone took the name, try the next
            Err(e) => return Err(alloc::format!("couldn't save it in {dir}: {}", e.name())),
        }
    }
    Err(String::from("no free file name"))
}

fn run(
    kind: Kind,
    spec: Spec,
    jar: Arc<Mutex<Jar>>,
    shared: Arc<Mutex<Shared>>,
    downloads: String,
) {
    if let Some(sc) = spec.script {
        run_script(sc, jar, shared);
        return;
    }
    let mut net = BrowserNet {
        inner: RtNet::new(Want::Any),
        client: None,
        shared: shared.clone(),
    };
    let mut hooks = BrowserHooks {
        kind,
        downloads: downloads.clone(),
        file: None,
        stream_cap: spec.stream_cap,
        reason: String::new(),
        caps: spec.caps,
        jar,
        ctx: spec.ctx,
        shared: shared.clone(),
        chain: spec.chain,
        base: spec.req.url.clone(),
    };
    let r = http::fetch(&mut net, &spec.req, &options(kind, spec.caps), &mut hooks);
    if matches!(kind, Kind::Page | Kind::Frame)
        && let Some(a) = net.inner.peer
    {
        shared.lock().peer = Some(match a.ip.unmapped() {
            rt::net::Ip::V4(b) => b.to_vec(),
            rt::net::Ip::V6(b) => b.to_vec(),
        });
    }
    let name = shared.lock().download.clone();
    let refused = shared.lock().refused.take();
    // A download written to disk as it came: closed now, and removed again if it did not
    // come whole (a stopped or broken download leaves no half file).
    let file = hooks.file.take();
    let out = match (r, name) {
        (_, _) if refused.is_some() => {
            if let Some(f) = file {
                discard_download(f);
            }
            Outcome::Refused(refused.unwrap())
        }
        (Ok(_), _) if file.is_some() => {
            let f = file.unwrap();
            match f.err.clone() {
                Some(e) => {
                    discard_download(f);
                    Outcome::SaveFailed(e)
                }
                None => {
                    let _ = sys::close(f.fd);
                    Outcome::Saved(f.path, f.n)
                }
            }
        }
        (Err(fail), _) if file.is_some() => {
            discard_download(file.unwrap());
            Outcome::Failed(fail)
        }
        (Ok(resp), _) if kind == Kind::Image => {
            if resp.status >= 400 {
                Outcome::Image(Err(alloc::format!("{} {}", resp.status, resp.reason)))
            } else {
                let err = |e: imgdec::ImgError| String::from(e.message());
                Outcome::Image(match imgdec::probe(&resp.body) {
                    Ok(info) if info.animated => imgdec::anim::Animation::new(resp.body)
                        .map(Picture::Moving)
                        .map_err(err),
                    Ok(_) => imgdec::decode(&resp.body).map(Picture::Still).map_err(err),
                    Err(e) => Err(err(e)),
                })
            }
        }
        (Ok(resp), _) => Outcome::Page(resp),
        (Err(f), _) => Outcome::Failed(f),
    };
    shared.lock().done = Some(out);
}

/// How far ahead of what a page has used the job sends a streamed body.
const SCRIPT_WINDOW: u64 = 1 << 20;

struct ScriptEvents {
    shared: Arc<Mutex<Shared>>,
    stream: bool,
}

impl scriptnet::Events for ScriptEvents {
    fn head(&mut self, head: &NetHead) {
        self.shared.lock().head = Some(head.clone());
    }
    fn progress(&mut self, got: usize, total: Option<usize>) {
        let mut s = self.shared.lock();
        s.got = got;
        s.total = total;
    }
    fn cancelled(&mut self) -> bool {
        self.shared.lock().cancel
    }
    fn wants_stream(&mut self) -> bool {
        self.stream
    }
    fn chunk(&mut self, data: &[u8]) -> bool {
        loop {
            {
                let mut s = self.shared.lock();
                if s.cancel {
                    return false;
                }
                // The page uses what it has; while it is behind by a window, the connection
                // waits and so does the server (a body of any length, read as slowly as the
                // page likes, never piles up here).
                if (s.stream.total as u64).saturating_sub(s.acked) < SCRIPT_WINDOW {
                    s.stream.total += data.len();
                    s.stream.chunks.push_back(data.to_vec());
                    return true;
                }
            }
            sys::sleep_ms(2);
        }
    }
}

struct JarAccess(Arc<Mutex<Jar>>);

impl scriptnet::Cookies for JarAccess {
    fn header(&self, url: &Url, post: bool, now_secs: i64, ctx: &Context) -> Option<String> {
        self.0.lock().header(url, post, now_secs, ctx)
    }
    fn store(&self, url: &Url, headers: &[(String, String)], now_secs: i64, ctx: &Context) {
        self.0.lock().set_all(url, headers, now_secs, ctx);
    }
}

struct CacheAccess(Arc<Mutex<PreflightCache>>);

impl scriptnet::Preflights for CacheAccess {
    fn with(&self, f: &mut dyn FnMut(&mut PreflightCache)) {
        f(&mut self.0.lock());
    }
}

/// A script's request, through the network policy, on this thread.
fn run_script(sc: ScriptSpec, jar: Arc<Mutex<Jar>>, shared: Arc<Mutex<Shared>>) {
    let mut net = BrowserNet {
        inner: RtNet::new(Want::Any),
        client: None,
        shared: shared.clone(),
    };
    let jar = JarAccess(jar);
    let cache = CacheAccess(sc.cache);
    let env = scriptnet::Env {
        cfg: &sc.cfg,
        page: &sc.page,
        jar: &jar,
        cache: &cache,
        now_secs: &now_secs,
    };
    let mut ev = ScriptEvents {
        shared: shared.clone(),
        stream: sc.req.stream,
    };
    let r = scriptnet::run(&mut net, &env, &sc.req, &mut ev);
    shared.lock().done = Some(Outcome::Net(Box::new(r)));
}

/// The running and waiting jobs.
pub struct Fetcher {
    jobs: Vec<Job>,
    waiting: VecDeque<JobId>,
    next: JobId,
    pub jar: Arc<Mutex<Jar>>,
    pub downloads: String,
    /// What the memory plan allows an image, a stylesheet and a script to be (set by the app
    /// as memory changes; a job takes the numbers when it is started).
    pub caps: (usize, usize, usize, usize),
    /// What a finished page job still had waiting to be taken (its last pieces) and whether it
    /// was cut at its cap, by job: the window asks for it once with [`Fetcher::take_leftover`].
    leftover: alloc::collections::BTreeMap<JobId, (Vec<u8>, Option<PageHead>)>,
}

impl Fetcher {
    pub fn new(jar: Jar, downloads: String) -> Fetcher {
        Fetcher {
            jobs: Vec::new(),
            waiting: VecDeque::new(),
            next: 1,
            jar: Arc::new(Mutex::new(jar)),
            downloads,
            caps: (MAX_IMAGE, MAX_STYLE, MAX_SCRIPT, MAX_PAGE),
            leftover: alloc::collections::BTreeMap::new(),
        }
    }

    /// The last pieces of a streamed page that finished since the window last looked, and its
    /// head (if it came and the window has not taken it).
    pub fn take_leftover(&mut self, id: JobId) -> (Vec<u8>, Option<PageHead>) {
        self.leftover.remove(&id).unwrap_or_default()
    }

    /// Start (or queue) `req`.
    pub fn start(&mut self, kind: Kind, req: Request, ctx: Context) -> JobId {
        self.start_with(kind, req, ctx, None)
    }

    fn start_with(&mut self, kind: Kind, req: Request, ctx: Context, stream_cap: Option<usize>) -> JobId {
        let id = self.next;
        self.next += 1;
        self.jobs.push(Job {
            id,
            kind,
            shared: Arc::new(Mutex::new(Shared::default())),
            handle: None,
            spec: Some(Spec {
                req,
                ctx,
                stream_cap,
                caps: self.caps,
                script: None,
                chain: None,
            }),
        });
        // Pages, stylesheets and scripts go first: they jump the queue of images.
        if matches!(kind, Kind::Page | Kind::Style | Kind::Script | Kind::Frame) {
            self.waiting.push_front(id);
        } else {
            self.waiting.push_back(id);
        }
        self.pump();
        id
    }

    /// Start (or queue) a page for a tab whose body streams in: it comes to the window's thread
    /// as it arrives (see [`Shared::stream`]), up to `cap` bytes.
    pub fn start_page(&mut self, req: Request, ctx: Context, cap: usize) -> JobId {
        self.start_with(Kind::Page, req, ctx, Some(cap))
    }

    /// Start (or queue) a frame's document. `chain`: the ancestors' addresses (top first), for
    /// the checks on every redirect hop.
    pub fn start_frame(&mut self, req: Request, ctx: Context, chain: Vec<Option<Url>>) -> JobId {
        let id = self.start(Kind::Frame, req, ctx);
        if let Some(j) = self.jobs.iter_mut().find(|j| j.id == id)
            && let Some(s) = j.spec.as_mut()
        {
            s.chain = Some(chain);
        }
        id
    }

    /// Start (or queue) a script's request.
    pub fn start_script(&mut self, sc: ScriptSpec) -> JobId {
        let id = self.next;
        self.next += 1;
        let url = Url::parse(&sc.req.url).unwrap_or_else(|_| sc.page.url.clone());
        self.jobs.push(Job {
            id,
            kind: Kind::Request,
            shared: Arc::new(Mutex::new(Shared::default())),
            handle: None,
            spec: Some(Spec {
                req: Request::get(&url),
                ctx: Context::TopLevel { from: None },
                stream_cap: None,
                caps: self.caps,
                script: Some(sc),
                chain: None,
            }),
        });
        self.waiting.push_back(id);
        self.pump();
        id
    }

    fn running(&self, scripts: bool) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.handle.is_some() && (j.kind == Kind::Request) == scripts)
            .count()
    }

    fn pump(&mut self) {
        let mut at = 0;
        while at < self.waiting.len() {
            let id = self.waiting[at];
            let scripts = self
                .jobs
                .iter()
                .find(|j| j.id == id)
                .is_some_and(|j| j.kind == Kind::Request);
            let room = if scripts {
                self.running(true) < MAX_RUNNING_REQUESTS
            } else {
                self.running(false) < MAX_RUNNING
            };
            if !room {
                at += 1;
                continue;
            }
            self.waiting.remove(at);
            let downloads = self.downloads.clone();
            let jar = self.jar.clone();
            let Some(j) = self.jobs.iter_mut().find(|j| j.id == id) else {
                continue;
            };
            let Some(spec) = j.spec.take() else {
                continue;
            };
            if j.shared.lock().cancel {
                j.shared.lock().done = Some(Outcome::Failed(Failure {
                    error: Error::Cancelled,
                    url: spec.req.url.clone(),
                    redirects: Vec::new(),
                }));
                continue;
            }
            let shared = j.shared.clone();
            let kind = j.kind;
            let mut work = Some((spec, jar, shared, downloads));
            j.handle = Some(thread::spawn_sized(STACK, move || {
                if let Some((spec, jar, shared, downloads)) = work.take() {
                    run(kind, spec, jar, shared, downloads);
                }
            }));
        }
    }

    pub fn shared(&self, id: JobId) -> Option<Arc<Mutex<Shared>>> {
        self.jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| j.shared.clone())
    }

    /// Ask a job to stop (its thread ends at its next read; its outcome is thrown away).
    pub fn cancel(&mut self, id: JobId) {
        if let Some(j) = self.jobs.iter().find(|j| j.id == id) {
            j.shared.lock().cancel = true;
        }
    }

    /// Finished jobs: their ids and outcomes (threads joined). Call it every tick.
    pub fn finished(&mut self) -> Vec<(JobId, Kind, Outcome, Option<CertInfo>, Option<NetHead>, Option<Vec<u8>>)> {
        let mut out = Vec::new();
        let mut k = 0;
        while k < self.jobs.len() {
            let done = {
                let mut s = self.jobs[k].shared.lock();
                s.done.take().map(|o| (o, s.cert.take(), s.head.take(), s.peer.take()))
            };
            match done {
                Some((o, cert, head, peer)) => {
                    {
                        let mut s = self.jobs[k].shared.lock();
                        if s.stream.total > 0 || s.page_head.is_some() {
                            let mut rest = Vec::new();
                            for c in core::mem::take(&mut s.stream.chunks) {
                                rest.extend(c);
                            }
                            s.stream.waiting = 0;
                            self.leftover.insert(self.jobs[k].id, (rest, s.page_head.take()));
                        }
                    }
                    let j = self.jobs.remove(k);
                    if let Some(h) = j.handle {
                        h.join();
                    }
                    out.push((j.id, j.kind, o, cert, head, peer));
                }
                None => k += 1,
            }
        }
        if !out.is_empty() {
            self.pump();
        }
        out
    }

    /// Pages, pictures and downloads are being fetched (script requests are not counted:
    /// [`Fetcher::requests`]).
    pub fn busy(&self) -> bool {
        self.jobs.iter().any(|j| j.kind != Kind::Request)
    }

    /// Script requests are outstanding.
    pub fn requests(&self) -> bool {
        self.jobs.iter().any(|j| j.kind == Kind::Request)
    }
}
