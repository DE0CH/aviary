//! The tx rule's clippy configuration (platform 4.3: no network call inside `aviary_pg::tx!`), generated from the
//! tree so it follows the code: the repository's `clippy.toml` plus every network call as a disallowed method.
//!
//! CI's lint job writes it with `cargo run -p aviary-source-rules --bin tx-rule-conf -- target/tx-rule` and runs a
//! second clippy pass with `CLIPPY_CONF_DIR=target/tx-rule … -A clippy::disallowed_methods`; the `aviary_pg::tx!`
//! macro marks its closure `#[deny(clippy::disallowed_methods)]`, so the lint fires only there. The calls:
//! - `reqwest`'s ([`REQWEST`]);
//! - the Kubernetes API's (`kube`, [`KUBE`]), and `hyper`'s, `hyper-util`'s and `tokio`'s sockets ([`LOW_LEVEL`]);
//! - every `pub async fn` of an inherent `impl` in a contract crate's typed client (`crates/*-api/src/client.rs`);
//! - `aviary_core::http::send`, `aws-config`'s loaders, and reading the AWS SDKs' paginators and byte streams
//!   ([`HELPERS`]);
//! - every operation of each of [`SDKS`] the workspace depends on (`send`, `send_with`, `customize().send()`, every
//!   waiter's `wait`), read from the SDK's sources.
//!
//! Not seen: a call made in a helper function that the closure calls (the lint level belongs to the helper's own
//! code), and a client this list does not know.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The reason clippy prints.
pub const REASON: &str = "no network call inside aviary_pg::tx!: commit first, then call out (platform 4.3)";

/// `reqwest`'s calls.
pub const REQWEST: &[&str] = &[
    "reqwest::Client::get",
    "reqwest::Client::post",
    "reqwest::Client::put",
    "reqwest::Client::patch",
    "reqwest::Client::delete",
    "reqwest::Client::head",
    "reqwest::Client::request",
    "reqwest::Client::execute",
    "reqwest::RequestBuilder::send",
    "reqwest::Response::bytes",
    "reqwest::Response::text",
    "reqwest::Response::json",
    "reqwest::Response::chunk",
    "reqwest::Response::bytes_stream",
    "reqwest::Response::text_with_charset",
    "reqwest::Response::upgrade",
    "reqwest::get",
];

/// The Kubernetes API (hermit): every `pub async fn` of `kube::Api` and `kube::Client` in kube-client 5.0.0 (the
/// workspace pins it exactly; re-check this list when the pin moves), and `kube::runtime`'s one-shot waits. A
/// `kube::runtime::watcher` stream polled with `StreamExt::next` is generic and not seen.
pub const KUBE: &[&str] = &[
    "kube::Api::get",
    "kube::Api::get_metadata",
    "kube::Api::get_with",
    "kube::Api::get_metadata_with",
    "kube::Api::get_opt",
    "kube::Api::get_metadata_opt",
    "kube::Api::get_metadata_opt_with",
    "kube::Api::list",
    "kube::Api::list_metadata",
    "kube::Api::create",
    "kube::Api::delete",
    "kube::Api::delete_collection",
    "kube::Api::patch",
    "kube::Api::patch_metadata",
    "kube::Api::replace",
    "kube::Api::watch",
    "kube::Api::watch_metadata",
    "kube::Api::entry",
    "kube::Api::get_scale",
    "kube::Api::patch_scale",
    "kube::Api::replace_scale",
    "kube::Api::get_subresource",
    "kube::Api::create_subresource",
    "kube::Api::patch_subresource",
    "kube::Api::replace_subresource",
    "kube::Api::get_ephemeral_containers",
    "kube::Api::patch_ephemeral_containers",
    "kube::Api::replace_ephemeral_containers",
    "kube::Api::get_resize",
    "kube::Api::patch_resize",
    "kube::Api::replace_resize",
    "kube::Api::get_status",
    "kube::Api::patch_status",
    "kube::Api::replace_status",
    "kube::Api::logs",
    "kube::Api::log_stream",
    "kube::Api::evict",
    "kube::Api::attach",
    "kube::Api::exec",
    "kube::Api::portforward",
    "kube::api::entry::Entry::commit",
    "kube::Client::send",
    "kube::Client::connect",
    "kube::Client::request",
    "kube::Client::request_text",
    "kube::Client::request_stream",
    "kube::Client::request_status",
    "kube::Client::request_events",
    "kube::Client::apiserver_version",
    "kube::Client::list_api_groups",
    "kube::Client::list_api_group_resources",
    "kube::Client::list_core_api_versions",
    "kube::Client::list_core_api_resources",
    "kube::Client::list_api_groups_aggregated",
    "kube::Client::list_core_api_versions_aggregated",
    "kube::runtime::wait::await_condition",
];

/// Lower-level clients: `hyper`'s connections and `hyper-util`'s pooled client, and opening a socket with `tokio`.
pub const LOW_LEVEL: &[&str] = &[
    "hyper::client::conn::http1::handshake",
    "hyper::client::conn::http1::SendRequest::send_request",
    "hyper::client::conn::http1::SendRequest::try_send_request",
    "hyper::client::conn::http1::SendRequest::ready",
    "hyper::client::conn::http2::handshake",
    "hyper::client::conn::http2::SendRequest::send_request",
    "hyper::client::conn::http2::SendRequest::try_send_request",
    "hyper::client::conn::http2::SendRequest::ready",
    "hyper_util::client::legacy::Client::request",
    "hyper_util::client::legacy::Client::get",
    "tokio::net::TcpStream::connect",
    "tokio::net::TcpSocket::connect",
    "tokio::net::UdpSocket::connect",
    "tokio::net::UdpSocket::send_to",
    "tokio::net::UdpSocket::recv_from",
    "tokio::net::lookup_host",
];

/// The other network calls listed by hand.
pub const HELPERS: &[&str] = &[
    "aviary_core::http::send",
    "aws_config::load_from_env",
    "aws_config::load_defaults",
    "aws_config::ConfigLoader::load",
    "aws_smithy_async::future::pagination_stream::PaginationStream::next",
    "aws_smithy_async::future::pagination_stream::PaginationStream::try_next",
    "aws_smithy_async::future::pagination_stream::PaginationStream::collect",
    "aws_smithy_types::byte_stream::ByteStream::next",
    "aws_smithy_types::byte_stream::ByteStream::try_next",
    "aws_smithy_types::byte_stream::ByteStream::collect",
];

/// The SDKs whose every operation is a network call.
pub const SDKS: &[&str] = &["aws-sdk-s3", "aws-sdk-ec2"];

/// One of [`SDKS`] in the workspace's graph.
#[derive(Debug, Clone)]
pub struct Sdk {
    /// The package name.
    pub name: &'static str,
    /// Its `src` directory.
    pub src: PathBuf,
}

/// Each of [`SDKS`] that the workspace depends on, from `cargo metadata` output. An SDK enters the list when a crate
/// first depends on it (hermit, EC2).
#[must_use]
pub fn sdks(metadata: &serde_json::Value) -> Vec<Sdk> {
    let Some(packages) = metadata["packages"].as_array() else {
        return Vec::new();
    };
    SDKS.iter()
        .filter_map(|name| {
            let p = packages.iter().find(|p| p["name"] == *name)?;
            let manifest = p["manifest_path"].as_str()?;
            Some(Sdk {
                name,
                src: Path::new(manifest).parent()?.join("src"),
            })
        })
        .collect()
}

/// `crate::client::Type::method` for every `pub async fn` of an inherent `impl` in a contract crate's `client.rs`.
///
/// # Errors
/// When `crates/` cannot be read.
pub fn typed_client_methods(root: &Path) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    for dir in sorted_dir(&root.join("crates"))? {
        let Some(dir_name) = dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !dir_name.ends_with("-api") {
            continue;
        }
        let Ok(src) = fs::read_to_string(dir.join("src/client.rs")) else {
            continue;
        };
        let krate = dir_name.replace('-', "_");
        let mut ty: Option<String> = None;
        for line in src.lines() {
            if let Some(rest) = line.strip_prefix("impl") {
                ty = inherent_impl_type(rest);
            } else if line.starts_with('}') {
                ty = None;
            } else if let (Some(t), Some(rest)) = (&ty, line.strip_prefix("    pub async fn ")) {
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                out.push(format!("{krate}::client::{t}::{name}"));
            }
        }
    }
    Ok(out)
}

/// The type of `impl[<…>] Type[<…>] {` (an inherent impl), `None` for a trait impl.
fn inherent_impl_type(rest: &str) -> Option<String> {
    if rest.contains(" for ") || !rest.trim_end().ends_with('{') {
        return None;
    }
    let mut s = rest.trim_start();
    if s.starts_with('<') {
        let mut depth = 0;
        let end = s
            .char_indices()
            .find(|(_, c)| {
                match c {
                    '<' => depth += 1,
                    '>' => depth -= 1,
                    _ => {}
                }
                depth == 0
            })
            .map(|(i, _)| i)?;
        s = s[end + 1..].trim_start();
    }
    let name: String = s.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    (!name.is_empty()).then_some(name)
}

fn sorted_dir(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut v = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    v.sort();
    Ok(v)
}

fn struct_name(line: &str, suffix: &str) -> Option<String> {
    let rest = line.strip_prefix("pub struct ")?;
    let name = rest.split([' ', '{', '<', '(', ';']).next()?;
    name.ends_with(suffix).then(|| name.to_owned())
}

/// Every operation of `sdk`: `<Op>FluentBuilder::send`, `<Op>InputBuilder::send_with`, `customize().send()`, and
/// every waiter's `wait`.
///
/// # Errors
/// When the SDK's sources cannot be read.
pub fn sdk_methods(sdk: &Sdk) -> io::Result<Vec<String>> {
    let krate = sdk.name.replace('-', "_");
    let mut out = vec![format!("{krate}::client::customize::CustomizableOperation::send")];
    for op in sorted_dir(&sdk.src.join("operation"))?
        .into_iter()
        .filter(|p| p.is_dir())
    {
        let Some(module) = op.file_name().and_then(|n| n.to_str()).map(str::to_owned) else {
            continue;
        };
        let builders = fs::read_to_string(op.join("builders.rs"))?;
        for line in builders.lines() {
            if let Some(name) = struct_name(line, "FluentBuilder") {
                out.push(format!("{krate}::operation::{module}::builders::{name}::send"));
            }
            if let Some(path) = line
                .strip_prefix("impl crate::operation::")
                .and_then(|r| r.strip_suffix(" {"))
                && path.ends_with("InputBuilder")
                && builders.contains("pub async fn send_with(")
            {
                out.push(format!("{krate}::operation::{path}::send_with"));
            }
        }
    }
    let waiters = sdk.src.join("waiters");
    if waiters.is_dir() {
        for w in sorted_dir(&waiters)? {
            let Some(module) = w.file_stem().and_then(|n| n.to_str()).map(str::to_owned) else {
                continue;
            };
            let code = fs::read_to_string(&w)?;
            if !code.contains("pub async fn wait(") {
                continue;
            }
            for name in code.lines().filter_map(|l| struct_name(l, "FluentBuilder")) {
                out.push(format!("{krate}::waiters::{module}::{name}::wait"));
            }
        }
    }
    Ok(out)
}

/// The tx rule's configuration: [`HEADER`], the repository's `clippy.toml`, then every network call.
///
/// # Errors
/// When a file cannot be read or `clippy.toml` does not end with the `disallowed-methods` list.
pub fn tx_rule_conf(root: &Path, metadata: &serde_json::Value) -> io::Result<String> {
    let main = fs::read_to_string(root.join("clippy.toml"))?;
    let main = main.trim_end().strip_suffix(']').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "clippy.toml does not end with the disallowed-methods list",
        )
    })?;
    let mut paths: Vec<String> = REQWEST
        .iter()
        .chain(KUBE)
        .chain(LOW_LEVEL)
        .chain(HELPERS)
        .map(|s| (*s).to_owned())
        .collect();
    paths.extend(typed_client_methods(root)?);
    for sdk in sdks(metadata) {
        paths.extend(sdk_methods(&sdk)?);
    }
    let mut out = format!("{HEADER}{main}  # the tx rule\n");
    for path in paths {
        let _ = writeln!(
            out,
            "  {{ path = \"{path}\", reason = \"{REASON}\", allow-invalid = true }},"
        );
    }
    out.push_str("]\n");
    Ok(out)
}

/// The generated file's first lines.
pub const HEADER: &str = "\
# GENERATED by aviary-source-rules (src/tx_rule.rs): the repository's clippy.toml plus every network call, for the
# second clippy pass that rejects network calls inside aviary_pg::tx! (platform 4.3). `allow-invalid`: a crate that
# does not depend on one cannot resolve its paths.

";
