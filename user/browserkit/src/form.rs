//! Sending forms: the page renderer works out a submission (method, address, the
//! urlencoded body); this turns it into the request, refusing anything that isn't an
//! http:// or https:// address.

use alloc::string::String;

use milkweb::http::Request;

use crate::address::{self, Target};
use crate::engine::{Method, Submission};

/// The request for a form submission. Err: why it can't be sent (a `javascript:` action,
/// another scheme).
pub fn request(sub: &Submission) -> Result<Request, String> {
    let url = match address::resolve(None, &sub.url) {
        Target::Web(u, _) => u,
        Target::Bad(why) => return Err(why),
        _ => return Err(String::from("a form can't be sent there")),
    };
    Ok(match sub.method {
        Method::Get => Request::get(&url),
        Method::Post => {
            let mut r = Request::post_form(&url, sub.body.clone());
            r.content_type = String::from(sub.content_type);
            r
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ControlKind, Document, Viewport};
    use crate::font::Unifont;

    fn submit(html: &str, setup: impl Fn(&mut Document)) -> Request {
        let mut d = Document::parse(
            html.as_bytes(),
            "http://milk.test/form/page?old=1",
            Some("text/html"),
        );
        d.layout(&Viewport::new(800, 600), &Unifont);
        setup(&mut d);
        let button = d
            .controls()
            .iter()
            .find(|c| c.kind == ControlKind::Submit)
            .map(|c| c.id)
            .unwrap();
        request(&d.submit(button, None).unwrap()).unwrap()
    }

    #[test]
    fn get_and_post() {
        let r = submit(
            "<form action=search><input name=q><input type=submit></form>",
            |d| {
                let id = d.controls()[0].id;
                d.set_value(id, "two words & more");
            },
        );
        assert_eq!(r.method, milkweb::http::Method::Get);
        assert_eq!(
            r.url.to_text(),
            "http://milk.test/form/search?q=two+words+%26+more"
        );
        let r = submit(
            "<form action=https://milk.test/login method=post><input name=user value=milk><input type=password name=pw><button>Go</button></form>",
            |d| {
                let id = d.controls()[1].id;
                d.set_value(id, "s3cr3t!");
            },
        );
        assert_eq!(r.method, milkweb::http::Method::Post);
        assert_eq!(r.url.to_text(), "https://milk.test/login");
        assert_eq!(r.body, b"user=milk&pw=s3cr3t%21");
        assert_eq!(r.content_type, "application/x-www-form-urlencoded");
    }

    #[test]
    fn refused_actions() {
        let mut d = Document::parse(
            b"<form action='javascript:alert(1)'><input type=submit></form>",
            "http://milk.test/",
            Some("text/html"),
        );
        d.layout(&Viewport::new(800, 600), &Unifont);
        let id = d.controls()[0].id;
        if let Some(sub) = d.submit(id, None) {
            assert!(request(&sub).is_err());
        }
    }
}
