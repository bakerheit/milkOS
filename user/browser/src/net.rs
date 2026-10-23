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
use imgdec::Image;
use milkweb::http::{
    self, Error, Failure, Hooks, Method, Net, Options, Request, Response, Transport,
};
use milkweb::url::Url;
use rt::net::Want;
use rt::sys;
use rt::thread::{self, JoinHandle, Mutex};
use rt::web::RtNet;

pub type JobId = u64;

/// The most a page may be (as received and uncompressed).
pub const MAX_PAGE: usize = 4 << 20;
/// The most a stylesheet may be.
pub const MAX_STYLE: usize = 2 << 20;
/// The most an image file may be.
pub const MAX_IMAGE: usize = 8 << 20;
/// The most a download may be (it is held in memory until it is written).
pub const MAX_DOWNLOAD: usize = 64 << 20;
/// Jobs running at once (more wait their turn).
pub const MAX_RUNNING: usize = 6;
const STACK: usize = 512 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// A page for a tab (it may turn out to be a download).
    Page,
    Image,
    /// A stylesheet for a page.
    Style,
    /// Save whatever comes (Alt-click on a link).
    Download,
}

pub enum Outcome {
    Page(Response),
    Image(Result<Image, String>),
    /// A download was written: its path and size.
    Saved(String, usize),
    Failed(Failure),
    /// A download came, but couldn't be written (why).
    SaveFailed(String),
}

/// What a job and the window share.
#[derive(Default)]
pub struct Shared {
    pub got: usize,
    pub total: Option<usize>,
    /// The window asks the job to stop.
    pub cancel: bool,
    /// The response is a file to save, under this name.
    pub download: Option<String>,
    /// The secure connection's details (https).
    pub cert: Option<CertInfo>,
    pub done: Option<Outcome>,
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

pub fn now_secs() -> i64 {
    (sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000) as i64
}

struct BrowserHooks {
    kind: Kind,
    jar: Arc<Mutex<Jar>>,
    ctx: Context,
    shared: Arc<Mutex<Shared>>,
}

fn is_redirect(status: u16, headers: &[(String, String)]) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308) && headers.iter().any(|(k, _)| k == "location")
}

impl Hooks for BrowserHooks {
    fn cookie(&mut self, url: &Url, method: Method) -> Option<String> {
        self.jar
            .lock()
            .header(url, method == Method::Post, now_secs(), &self.ctx)
    }

    fn response(&mut self, url: &Url, status: u16, headers: &[(String, String)]) -> Option<usize> {
        self.jar.lock().set_all(url, headers, now_secs(), &self.ctx);
        if is_redirect(status, headers) {
            return None;
        }
        let h = |n: &str| {
            headers
                .iter()
                .find(|(k, _)| k == n)
                .map(|(_, v)| v.as_str())
        };
        match self.kind {
            Kind::Image => Some(MAX_IMAGE),
            Kind::Style => None,
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
                Some(MAX_DOWNLOAD)
            }
        }
    }

    fn progress(&mut self, got: usize, total: Option<usize>) {
        let mut s = self.shared.lock();
        s.got = got;
        s.total = total;
    }

    fn stop(&mut self) -> bool {
        self.shared.lock().cancel
    }
}

fn options(kind: Kind) -> Options {
    Options {
        max_body: match kind {
            Kind::Image => MAX_IMAGE,
            Kind::Style => MAX_STYLE,
            _ => MAX_PAGE,
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
            _ => "text/html, application/xhtml+xml, text/plain;q=0.9, */*;q=0.8",
        }),
        // Downloads come as they are (the file is the file).
        decompress: kind != Kind::Download,
        ..Options::default()
    }
}

/// Write a download into `dir` under a free name: the path.
fn save(dir: &str, name: &str, bytes: &[u8]) -> Result<String, String> {
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
            Ok(fd) => {
                let r = sys::write_all(fd, bytes);
                let _ = sys::close(fd);
                return r
                    .map(|_| path)
                    .map_err(|e| alloc::format!("couldn't write it: {}", e.name()));
            }
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
    let mut net = BrowserNet {
        inner: RtNet::new(Want::Any),
        client: None,
        shared: shared.clone(),
    };
    let mut hooks = BrowserHooks {
        kind,
        jar,
        ctx: spec.ctx,
        shared: shared.clone(),
    };
    let r = http::fetch(&mut net, &spec.req, &options(kind), &mut hooks);
    let name = shared.lock().download.clone();
    let out = match (r, name) {
        (Ok(resp), Some(name)) if resp.status < 400 || kind == Kind::Download => {
            match save(&downloads, &name, &resp.body) {
                Ok(p) => Outcome::Saved(p, resp.body.len()),
                Err(e) => Outcome::SaveFailed(e),
            }
        }
        (Ok(resp), _) if kind == Kind::Image => {
            if resp.status >= 400 {
                Outcome::Image(Err(alloc::format!("{} {}", resp.status, resp.reason)))
            } else {
                Outcome::Image(imgdec::decode(&resp.body).map_err(|e| String::from(e.message())))
            }
        }
        (Ok(resp), _) => Outcome::Page(resp),
        (Err(f), _) => Outcome::Failed(f),
    };
    shared.lock().done = Some(out);
}

/// The running and waiting jobs.
pub struct Fetcher {
    jobs: Vec<Job>,
    waiting: VecDeque<JobId>,
    next: JobId,
    pub jar: Arc<Mutex<Jar>>,
    pub downloads: String,
}

impl Fetcher {
    pub fn new(jar: Jar, downloads: String) -> Fetcher {
        Fetcher {
            jobs: Vec::new(),
            waiting: VecDeque::new(),
            next: 1,
            jar: Arc::new(Mutex::new(jar)),
            downloads,
        }
    }

    /// Start (or queue) `req`.
    pub fn start(&mut self, kind: Kind, req: Request, ctx: Context) -> JobId {
        let id = self.next;
        self.next += 1;
        self.jobs.push(Job {
            id,
            kind,
            shared: Arc::new(Mutex::new(Shared::default())),
            handle: None,
            spec: Some(Spec { req, ctx }),
        });
        // Pages and stylesheets go first: they jump the queue of images.
        if matches!(kind, Kind::Page | Kind::Style) {
            self.waiting.push_front(id);
        } else {
            self.waiting.push_back(id);
        }
        self.pump();
        id
    }

    fn running(&self) -> usize {
        self.jobs.iter().filter(|j| j.handle.is_some()).count()
    }

    fn pump(&mut self) {
        while self.running() < MAX_RUNNING {
            let Some(id) = self.waiting.pop_front() else {
                return;
            };
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
    pub fn finished(&mut self) -> Vec<(JobId, Kind, Outcome, Option<CertInfo>)> {
        let mut out = Vec::new();
        let mut k = 0;
        while k < self.jobs.len() {
            let done = {
                let mut s = self.jobs[k].shared.lock();
                s.done.take().map(|o| (o, s.cert.take()))
            };
            match done {
                Some((o, cert)) => {
                    let j = self.jobs.remove(k);
                    if let Some(h) = j.handle {
                        h.join();
                    }
                    out.push((j.id, j.kind, o, cert));
                }
                None => k += 1,
            }
        }
        if !out.is_empty() {
            self.pump();
        }
        out
    }

    pub fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }
}
