//! Submitting forms: `application/x-www-form-urlencoded` (the HTML standard's algorithm) and
//! turning a submission into the request a GET or POST form makes.

use alloc::string::String;
use alloc::vec::Vec;

use milkweb::http::{Method, Request};
use milkweb::url::Url;

use crate::address::{self, Target};

/// What a form sends: method, where (`action`, as written; empty means the page itself) and
/// the name / value pairs in document order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub method: Method,
    pub action: String,
    pub pairs: Vec<(String, String)>,
}

fn encode_into(o: &mut String, s: &str) {
    // Newlines are normalised to CR LF first.
    let s = s
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\n', "\r\n");
    for &c in s.as_bytes() {
        match c {
            b' ' => o.push('+'),
            c if c.is_ascii_alphanumeric() || matches!(c, b'*' | b'-' | b'.' | b'_') => {
                o.push(c as char)
            }
            c => o.push_str(&alloc::format!("%{c:02X}")),
        }
    }
}

/// `a=1&b=two+words` (UTF-8, percent-encoded).
pub fn urlencode(pairs: &[(String, String)]) -> String {
    let mut o = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            o.push('&');
        }
        encode_into(&mut o, k);
        o.push('=');
        encode_into(&mut o, v);
    }
    o
}

/// The request for `sub` on page `base`: a GET replaces the action's query with the form's,
/// a POST sends it as the body. Err: why not (a `javascript:` action, ...).
pub fn request(sub: &Submission, base: &Url) -> Result<Request, String> {
    let action = if sub.action.trim().is_empty() {
        base.to_text()
    } else {
        String::from(sub.action.trim())
    };
    let url = match address::resolve(Some(base), &action) {
        Target::Web(u, _) => u,
        Target::Bad(why) => return Err(why),
        _ => return Err(String::from("a form can't be sent there")),
    };
    let data = urlencode(&sub.pairs);
    Ok(match sub.method {
        Method::Get => {
            let path = url.path.split('?').next().unwrap_or("/");
            let text = alloc::format!(
                "{}://{}{}?{}",
                url.scheme.name(),
                url.host_port(),
                path,
                data
            );
            let u = Url::parse(&text).map_err(|e| alloc::format!("{e}"))?;
            Request::get(&u)
        }
        Method::Post => Request::post_form(&url, data.into_bytes()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(k: &str, v: &str) -> (String, String) {
        (k.into(), v.into())
    }

    #[test]
    fn encoding() {
        assert_eq!(
            urlencode(&[
                p("q", "milk & honey"),
                p("n", "café"),
                p("x", "a=b/c?*-._~")
            ]),
            "q=milk+%26+honey&n=caf%C3%A9&x=a%3Db%2Fc%3F*-._%7E"
        );
        assert_eq!(urlencode(&[p("t", "a\nb\r\nc")]), "t=a%0D%0Ab%0D%0Ac");
        assert_eq!(urlencode(&[]), "");
    }

    #[test]
    fn requests() {
        let base = Url::parse("http://milk.test/form/page?old=1").unwrap();
        let get = Submission {
            method: Method::Get,
            action: "search?ignored=1".into(),
            pairs: alloc::vec![p("q", "two words")],
        };
        let r = request(&get, &base).unwrap();
        assert_eq!(r.method, Method::Get);
        assert_eq!(r.url.to_text(), "http://milk.test/form/search?q=two+words");
        let own = Submission {
            method: Method::Get,
            action: String::new(),
            pairs: alloc::vec![p("a", "1")],
        };
        assert_eq!(request(&own, &base).unwrap().url.path, "/form/page?a=1");
        let post = Submission {
            method: Method::Post,
            action: "https://milk.test/login".into(),
            pairs: alloc::vec![p("user", "milk"), p("pw", "s3cr3t!")],
        };
        let r = request(&post, &base).unwrap();
        assert_eq!(r.method, Method::Post);
        assert_eq!(r.url.to_text(), "https://milk.test/login");
        assert_eq!(r.body, b"user=milk&pw=s3cr3t%21");
        assert_eq!(r.content_type, "application/x-www-form-urlencoded");
        let js = Submission {
            action: "javascript:void(0)".into(),
            ..post
        };
        assert!(request(&js, &base).is_err());
    }
}
