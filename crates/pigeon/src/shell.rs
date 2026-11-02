//! The app shell (spec 5.2.2): `index.html` for every page path, signed in or not, changed only by the product
//! name in `<title>` and the app-title meta and, when nobody is signed in, the signed-out meta before `</head>`.
//! Every other byte is served as built (the early message box and its inline script included).

use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;

use crate::App;
use crate::http::{Page, page};
use crate::identity::{Identity, Sources};
use crate::paseo::compat;

const APP_TITLE_META: &str = "apple-mobile-web-app-title";

/// The shell and its two rendered variants per product name. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Shell(Arc<ShellInner>);

#[derive(Debug)]
struct ShellInner {
    index: Vec<u8>,
    rendered: Mutex<Option<Rendered>>,
}

#[derive(Debug, Clone)]
struct Rendered {
    product: String,
    signed_in: Bytes,
    signed_out: Bytes,
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn find(hay: &str, needle: &str, from: usize) -> Option<usize> {
    hay.get(from..)?.find(needle).map(|i| i + from)
}

/// Replaces the text of the first `<title>` element.
fn set_title(html: &str, title: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(open) = find(&lower, "<title", 0) else {
        return html.to_owned();
    };
    let (Some(gt), Some(close)) = (find(&lower, ">", open), find(&lower, "</title>", open)) else {
        return html.to_owned();
    };
    if gt > close {
        return html.to_owned();
    }
    format!("{}{}{}", &html[..=gt], title, &html[close..])
}

/// Replaces the `content` value of the `<meta name="apple-mobile-web-app-title">` tag.
fn set_app_title(html: &str, title: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(start) = find(&lower, "<meta", from) {
        let Some(end) = find(&lower, ">", start) else { break };
        let tag = &lower[start..end];
        if tag.contains(&format!("name=\"{APP_TITLE_META}\"")) {
            if let Some(c) = tag.find("content=\"") {
                let value_start = start + c + "content=\"".len();
                if let Some(value_end) = find(&lower, "\"", value_start).filter(|e| *e <= end) {
                    return format!("{}{}{}", &html[..value_start], title, &html[value_end..]);
                }
            }
            return html.to_owned();
        }
        from = end;
    }
    html.to_owned()
}

/// Inserts the signed-out meta before the first `</head>`.
fn signed_out(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    match lower.find("</head>") {
        Some(i) => format!("{}{}{}", &html[..i], compat::SIGNED_OUT_META, &html[i..]),
        None => html.to_owned(),
    }
}

/// The two variants of `index` for `product`: (signed in, signed out).
#[must_use]
pub fn render(index: &[u8], product: &str) -> (Vec<u8>, Vec<u8>) {
    let html = String::from_utf8_lossy(index);
    let name = escape(product);
    let named = set_app_title(&set_title(&html, &name), &name);
    let out = signed_out(&named);
    (named.into_bytes(), out.into_bytes())
}

impl Shell {
    pub(crate) fn new(index: Vec<u8>) -> Self {
        Self(Arc::new(ShellInner {
            index,
            rendered: Mutex::new(None),
        }))
    }

    fn variants(&self, product: &str) -> Rendered {
        if let Ok(r) = self.0.rendered.lock()
            && let Some(r) = r.as_ref()
            && r.product == product
        {
            return r.clone();
        }
        let (signed_in, signed_out) = render(&self.0.index, product);
        let r = Rendered {
            product: product.to_owned(),
            signed_in: Bytes::from(signed_in),
            signed_out: Bytes::from(signed_out),
        };
        if let Ok(mut slot) = self.0.rendered.lock() {
            *slot = Some(r.clone());
        }
        r
    }
}

/// Serves the shell for a page path. Identity comes only from the cookie (the edge sends no assertion on page
/// paths); a verified but not-allowed cookie gets the 403 page, anything not signed in the signed-out shell.
pub async fn serve(app: &App, headers: &HeaderMap) -> Response {
    let id = app.0.identity.resolve(headers, Sources::CookieOnly, None).await;
    let product = app.0.identity.product_name();
    let v = app.0.shell.variants(&product);
    let body = match &id {
        Identity::Person(_) | Identity::ViewAs(_) => v.signed_in,
        Identity::NotAllowed(email) => return page(app, Page::NotAllowed, Some(email)),
        Identity::SignedOut | Identity::ViewAsRefused | Identity::Unavailable => v.signed_out,
    };
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = StatusCode::OK;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::VARY, HeaderValue::from_static("cookie"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>App</title>\
        <meta name=\"apple-mobile-web-app-title\" content=\"App\"><script>early()</script></head>\
        <body><textarea id=\"early\"></textarea><script src=\"/_expo/static/js/a.js\"></script></body></html>";

    #[test]
    fn only_the_three_substitutions() {
        let (inn, out) = render(INDEX.as_bytes(), "Team <AI>");
        let inn = String::from_utf8(inn).unwrap();
        let out = String::from_utf8(out).unwrap();
        let expected_in = INDEX
            .replace("<title>App</title>", "<title>Team &lt;AI&gt;</title>")
            .replace("content=\"App\"", "content=\"Team &lt;AI&gt;\"");
        assert_eq!(inn, expected_in);
        assert_eq!(
            out,
            expected_in.replace("</head>", &format!("{}</head>", compat::SIGNED_OUT_META))
        );
    }

    #[test]
    fn a_shell_without_the_tags_is_unchanged() {
        let (inn, _) = render(b"<html><body>x</body></html>", "P");
        assert_eq!(inn, b"<html><body>x</body></html>");
    }
}
