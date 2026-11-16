//! The server as the page uses it: a real `yyc-drive` on a temporary folder, spoken to over HTTP.

use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

use reqwest::{
    blocking::{Client, RequestBuilder, Response},
    redirect::Policy,
    Method, StatusCode,
};

const DRIVE: &str = "00000000-0000-4000-8000-000000000001$00000000-0000-4000-8000-000000000001";

struct Server {
    child: Child,
    base: String,
    root: PathBuf,
    _dir: tempfile::TempDir,
    http: Client,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Server {
    /// `dev_user`: the person assumed when no header names one (None: a request without the header is refused).
    fn start(dev_user: Option<&str>) -> Server {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let web = dir.path().join("web");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(web.join("js")).unwrap();
        fs::write(web.join("index.html"), "<html>page</html>").unwrap();
        fs::write(web.join("js/app.js"), "js").unwrap();
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_yyc-drive"));
        cmd.env("DRIVE_ROOT", &root)
            .env("DRIVE_WEB", &web)
            .env("DRIVE_NAME", "Test Drive")
            .env("DRIVE_LISTEN", format!("127.0.0.1:{port}"))
            .env_remove("DRIVE_DEV_USER")
            .env_remove("DRIVE_URL")
            .stderr(Stdio::null());
        if let Some(u) = dev_user {
            cmd.env("DRIVE_DEV_USER", u);
        }
        let child = cmd.spawn().unwrap();
        let http = Client::builder().redirect(Policy::none()).timeout(Duration::from_secs(20)).build().unwrap();
        let base = format!("http://127.0.0.1:{port}");
        for _ in 0..100 {
            if http.get(format!("{base}/config.json")).send().is_ok() {
                return Server { child, base, root: root.canonicalize().unwrap(), _dir: dir, http };
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the server did not start");
    }

    fn req(&self, method: &str, path: &str) -> RequestBuilder {
        self.http.request(Method::from_bytes(method.as_bytes()).unwrap(), format!("{}{}", self.base, path))
    }

    fn dav(&self, method: &str, path: &str) -> RequestBuilder {
        self.req(method, &format!("/dav/spaces/{DRIVE}/{path}"))
    }

    fn dest(&self, path: &str) -> String {
        format!("{}/dav/spaces/{DRIVE}/{path}", self.base)
    }
}

fn text(r: Response) -> String {
    r.text().unwrap()
}

#[test]
fn the_page_and_its_settings() {
    let s = Server::start(Some("amy@example.com"));
    let config: String = text(s.req("GET", "/config.json").send().unwrap());
    assert!(config.contains(&format!("\"server\":\"{}/\"", s.base)), "{config}");
    assert!(config.contains("\"disableFeedbackLink\":true") && config.contains("\"disableSponsorLink\":true"));
    // Any path that is not a built file is the page itself.
    assert_eq!(text(s.req("GET", "/files/spaces/personal/drive").send().unwrap()), "<html>page</html>");
    let js = s.req("GET", "/js/app.js").send().unwrap();
    assert!(js.headers()["cache-control"].to_str().unwrap().contains("immutable"));
    assert_eq!(s.req("GET", "/.env").send().unwrap().status(), StatusCode::BAD_REQUEST);
}

#[test]
fn sign_in_is_answered_for_the_person_the_proxy_names() {
    let s = Server::start(None);
    let meta = text(s.req("GET", "/.well-known/openid-configuration").send().unwrap());
    assert!(meta.contains(&format!("\"authorization_endpoint\":\"{}/oidc/authorize\"", s.base)));
    let r = s.req("GET", "/oidc/authorize?redirect_uri=http%3A%2F%2Fx%2Foidc-callback.html&state=abc").send().unwrap();
    assert_eq!(r.status(), StatusCode::FOUND);
    assert_eq!(r.headers()["location"], "http://x/oidc-callback.html?code=drive&state=abc");
    // No header, no person: refused.
    assert_eq!(s.req("POST", "/oidc/token").send().unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(s.req("GET", "/graph/v1.0/me").send().unwrap().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(s.dav("PROPFIND", "").send().unwrap().status(), StatusCode::UNAUTHORIZED);
    let who = "cf-access-authenticated-user-email";
    let token = text(s.req("POST", "/oidc/token").header(who, "amy@example.com").send().unwrap());
    assert!(token.contains("\"id_token\"") && token.contains("\"access_token\""));
    let me = text(s.req("GET", "/graph/v1.0/me").header(who, "amy@example.com").send().unwrap());
    assert!(me.contains("\"mail\":\"amy@example.com\"") && me.contains("\"displayName\":\"amy\""), "{me}");
    let info = text(s.req("GET", "/oidc/userinfo").header(who, "amy@example.com").send().unwrap());
    assert!(info.contains("\"sub\":\"amy@example.com\""));
}

#[test]
fn one_drive_and_nothing_else_on_offer() {
    let s = Server::start(Some("amy@example.com"));
    let mine = text(s.req("GET", "/graph/v1beta1/me/drives?%24orderby=name+asc&%24filter=driveType+eq+personal").send().unwrap());
    assert!(mine.contains("\"name\":\"Test Drive\"") && mine.contains("\"driveType\":\"personal\""), "{mine}");
    let projects = text(s.req("GET", "/graph/v1beta1/me/drives?%24orderby=name+asc&%24filter=driveType+eq+project").send().unwrap());
    assert_eq!(projects, "{\"value\":[]}");
    let caps = text(s.req("GET", "/ocs/v1.php/cloud/capabilities?format=json").send().unwrap());
    for off in ["\"api_enabled\":false", "\"projects\":false", "\"favorites\":false", "\"undelete\":false", "\"support-sse\":false"] {
        assert!(caps.contains(off), "{off} missing in {caps}");
    }
    assert!(caps.contains("\"max_chunk_size\":10000000"));
    let key = text(s.req("GET", "/ocs/v1.php/cloud/user/signing-key").send().unwrap());
    assert!(key.starts_with("<?xml") && key.contains("<signing-key>"), "{key}");
}

#[test]
fn listing_shows_the_folder_and_hides_dot_names() {
    let s = Server::start(Some("amy@example.com"));
    fs::create_dir(s.root.join("影片 素材")).unwrap();
    fs::write(s.root.join("影片 素材/a&b.mp4"), b"0123456789").unwrap();
    fs::write(s.root.join(".hidden"), b"x").unwrap();
    let r = s.dav("PROPFIND", "").header("depth", "1").send().unwrap();
    assert_eq!(r.status(), StatusCode::MULTI_STATUS);
    let xml = text(r);
    assert!(xml.contains("<oc:name>Test Drive</oc:name>"));
    assert!(xml.contains("<oc:name>影片 素材</oc:name>") && xml.contains("<d:collection/>"));
    assert!(!xml.contains(".hidden") && !xml.contains(".drive-uploads"), "{xml}");
    let xml = text(s.dav("PROPFIND", "%E5%BD%B1%E7%89%87%20%E7%B4%A0%E6%9D%90").header("depth", "1").send().unwrap());
    assert!(xml.contains("<oc:name>a&amp;b.mp4</oc:name>"), "{xml}");
    assert!(xml.contains("<d:getcontentlength>10</d:getcontentlength>") && xml.contains("<d:getcontenttype>video/mp4</d:getcontenttype>"));
    // The page takes a file's address from this property; without it the viewer cannot load the file.
    assert!(xml.contains(&format!("<oc:downloadURL>{}/dav/spaces/{DRIVE}/%E5%BD%B1%E7%89%87%20%E7%B4%A0%E6%9D%90/a%26b.mp4</oc:downloadURL>", s.base)), "{xml}");
    // depth 0: the folder alone.
    let xml = text(s.dav("PROPFIND", "").header("depth", "0").send().unwrap());
    assert_eq!(xml.matches("<d:response>").count(), 1);
    assert_eq!(s.dav("PROPFIND", "nope").send().unwrap().status(), StatusCode::NOT_FOUND);
    assert_eq!(s.dav("PROPFIND", ".hidden").send().unwrap().status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_file_is_read_whole_or_by_byte_range() {
    let s = Server::start(Some("amy@example.com"));
    fs::write(s.root.join("v.mp4"), b"0123456789").unwrap();
    let r = s.dav("GET", "v.mp4").send().unwrap();
    assert_eq!(r.headers()["accept-ranges"], "bytes");
    assert_eq!(text(r), "0123456789");
    let r = s.dav("GET", "v.mp4").header("range", "bytes=2-5").send().unwrap();
    assert_eq!(r.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.headers()["content-range"], "bytes 2-5/10");
    assert_eq!(text(r), "2345");
    assert_eq!(text(s.dav("GET", "v.mp4").header("range", "bytes=7-").send().unwrap()), "789");
    assert_eq!(text(s.dav("GET", "v.mp4").header("range", "bytes=-3").send().unwrap()), "789");
    assert_eq!(s.dav("GET", "v.mp4").header("range", "bytes=10-").send().unwrap().status(), StatusCode::RANGE_NOT_SATISFIABLE);
}

#[test]
fn write_rename_copy_delete() {
    let s = Server::start(Some("amy@example.com"));
    assert_eq!(s.dav("PUT", "a.txt").body("one").send().unwrap().status(), StatusCode::CREATED);
    assert_eq!(s.dav("PUT", "a.txt").body("two").send().unwrap().status(), StatusCode::NO_CONTENT);
    assert_eq!(fs::read(s.root.join("a.txt")).unwrap(), b"two");
    assert_eq!(s.dav("MKCOL", "d").send().unwrap().status(), StatusCode::CREATED);
    assert_eq!(s.dav("MKCOL", "d").send().unwrap().status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(s.dav("MOVE", "a.txt").header("destination", s.dest("d/b.txt")).send().unwrap().status(), StatusCode::CREATED);
    assert!(!s.root.join("a.txt").exists() && s.root.join("d/b.txt").exists());
    assert_eq!(s.dav("COPY", "d").header("destination", s.dest("d%20copy")).send().unwrap().status(), StatusCode::CREATED);
    assert_eq!(fs::read(s.root.join("d copy/b.txt")).unwrap(), b"two");
    // An existing target is kept unless the request says to replace it.
    assert_eq!(s.dav("MOVE", "d").header("destination", s.dest("d%20copy")).send().unwrap().status(), StatusCode::PRECONDITION_FAILED);
    assert!(s.root.join("d").exists());
    assert_eq!(s.dav("MOVE", "d").header("destination", s.dest("d%20copy")).header("overwrite", "T").send().unwrap().status(), StatusCode::NO_CONTENT);
    assert!(!s.root.join("d").exists());
    // A folder cannot move into itself, and the drive itself cannot be moved or deleted.
    assert_eq!(s.dav("MOVE", "d%20copy").header("destination", s.dest("d%20copy/in")).send().unwrap().status(), StatusCode::CONFLICT);
    assert_eq!(s.dav("DELETE", "").send().unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(s.dav("DELETE", "d%20copy").send().unwrap().status(), StatusCode::NO_CONTENT);
    assert!(!s.root.join("d copy").exists());
    assert_eq!(s.dav("DELETE", "d%20copy").send().unwrap().status(), StatusCode::NOT_FOUND);
}

#[test]
fn a_path_cannot_leave_the_folder() {
    let s = Server::start(Some("amy@example.com"));
    let outside = s.root.parent().unwrap().join("secret.txt");
    fs::write(&outside, b"secret").unwrap();
    for p in ["..%2Fsecret.txt", "%2e%2e/secret.txt", "a/..%2F..%2Fsecret.txt", ".drive-uploads/x"] {
        assert_eq!(s.dav("GET", p).send().unwrap().status(), StatusCode::NOT_FOUND, "{p}");
        assert_eq!(s.dav("PUT", p).body("x").send().unwrap().status(), StatusCode::NOT_FOUND, "{p}");
        assert_eq!(s.dav("DELETE", p).send().unwrap().status(), StatusCode::NOT_FOUND, "{p}");
    }
    fs::write(s.root.join("a.txt"), b"a").unwrap();
    let r = s.dav("MOVE", "a.txt").header("destination", s.dest("..%2Fmoved.txt")).send().unwrap();
    assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fs::read(&outside).unwrap(), b"secret");
    assert!(s.root.join("a.txt").exists());
    // Another drive's id is not this drive.
    assert_eq!(s.req("PROPFIND", "/dav/spaces/other$drive/").send().unwrap().status(), StatusCode::NOT_FOUND);
}

fn meta(name: &str) -> String {
    use base64::Engine;
    format!("filename {}", base64::engine::general_purpose::STANDARD.encode(name))
}

#[test]
fn an_upload_arrives_in_pieces_and_can_resume() {
    let s = Server::start(Some("amy@example.com"));
    fs::create_dir(s.root.join("in")).unwrap();
    // The first piece rides on the request that starts the upload.
    let r = s.dav("POST", "in").header("upload-length", "10").header("upload-metadata", meta("片 1.bin")).header("tus-resumable", "1.0.0").body("0123").send().unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(r.headers()["upload-offset"], "4");
    let at = r.headers()["location"].to_str().unwrap().to_string();
    assert!(at.starts_with(&format!("{}/data/tus/", s.base)), "{at}");
    let at = at[s.base.len()..].to_string();
    // Not in the folder until the last byte is in, and not in a listing meanwhile.
    assert!(!s.root.join("in/片 1.bin").exists());
    assert!(!text(s.dav("PROPFIND", "").header("depth", "1").send().unwrap()).contains("drive-uploads"));
    // A piece at the wrong place is refused and the server says where it is.
    let r = s.req("PATCH", &at).header("upload-offset", "7").body("789").send().unwrap();
    assert_eq!(r.status(), StatusCode::CONFLICT);
    assert_eq!(r.headers()["upload-offset"], "4");
    let r = s.req("PATCH", &at).header("upload-offset", "4").body("456").send().unwrap();
    assert_eq!((r.status(), r.headers()["upload-offset"].to_str().unwrap()), (StatusCode::NO_CONTENT, "7"));
    // After a break the page asks how far the upload got.
    let r = s.req("HEAD", &at).send().unwrap();
    assert_eq!((r.headers()["upload-offset"].to_str().unwrap(), r.headers()["upload-length"].to_str().unwrap()), ("7", "10"));
    // More than was announced is refused.
    assert_eq!(s.req("PATCH", &at).header("upload-offset", "7").body("789AB").send().unwrap().status(), StatusCode::PAYLOAD_TOO_LARGE);
    let r = s.req("HEAD", &at).send().unwrap();
    let got: u64 = r.headers()["upload-offset"].to_str().unwrap().parse().unwrap();
    let rest = &b"0123456789"[got as usize..];
    let r = s.req("PATCH", &at).header("upload-offset", got.to_string()).body(rest.to_vec()).send().unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert!(r.headers().contains_key("oc-fileid"));
    assert_eq!(fs::read(s.root.join("in/片 1.bin")).unwrap(), b"0123456789");
    assert_eq!(fs::read_dir(s.root.join(".drive-uploads")).unwrap().count(), 0);
    assert_eq!(s.req("HEAD", &at).send().unwrap().status(), StatusCode::NOT_FOUND);
}

#[test]
fn an_upload_names_a_file_inside_the_folder_only() {
    let s = Server::start(Some("amy@example.com"));
    // A dropped folder arrives as names with their path below it.
    let r = s.dav("POST", "").header("upload-length", "2").header("upload-metadata", meta("new/deep/f.txt")).body("ok").send().unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(fs::read(s.root.join("new/deep/f.txt")).unwrap(), b"ok");
    for bad in ["../out.txt", "/etc/x", ".hidden", "a/../../x"] {
        let r = s.dav("POST", "").header("upload-length", "2").header("upload-metadata", meta(bad)).body("no").send().unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    assert!(!s.root.parent().unwrap().join("out.txt").exists());
    // An empty file is complete at once.
    let r = s.dav("POST", "").header("upload-length", "0").header("upload-metadata", meta("empty.txt")).send().unwrap();
    assert_eq!(r.status(), StatusCode::CREATED);
    assert_eq!(fs::read(s.root.join("empty.txt")).unwrap(), b"");
}

#[test]
fn search_finds_names_anywhere_in_the_folder() {
    let s = Server::start(Some("amy@example.com"));
    fs::create_dir_all(s.root.join("a/b")).unwrap();
    fs::write(s.root.join("a/b/主舞台 A.mp4"), b"x").unwrap();
    fs::write(s.root.join("a/Stage notes.txt"), b"x").unwrap();
    fs::write(s.root.join(".主舞台 hidden"), b"x").unwrap();
    let ask = |word: &str| {
        let body = format!("<oc:search-files xmlns:d=\"DAV:\" xmlns:oc=\"http://owncloud.org/ns\"><oc:search><oc:pattern>(name:&quot;*{word}*&quot; OR tag:&quot;*{word}*&quot;)</oc:pattern><oc:limit>200</oc:limit></oc:search></oc:search-files>");
        let r = s.req("REPORT", "/dav/spaces").body(body).send().unwrap();
        assert_eq!(r.status(), StatusCode::MULTI_STATUS);
        text(r)
    };
    let xml = ask("主舞台");
    assert_eq!(xml.matches("<d:response>").count(), 1, "{xml}");
    assert!(xml.contains("<oc:name>主舞台 A.mp4</oc:name>"));
    // Any letter case.
    assert!(ask("STAGE").contains("<oc:name>Stage notes.txt</oc:name>"));
    assert_eq!(ask("nothing-like-this").matches("<d:response>").count(), 0);
}
