//! EC2 for the AWS hosts (hermit 13.5, 13.6, 13.9, 14.4): `RunInstances` from the launch template, `TerminateInstances`,
//! `DescribeInstances` by hermit's own tags, through `aws-sdk-ec2` with the static credentials of IAM user
//! `hermit-scaler` (`HERMIT_AWS_ACCESS_KEY_ID/SECRET`); and the `WireGuard` key pair a new host gets in its user data.
//! The [`Ec2`] trait exists so tests run the scaler against a fake.

use std::net::IpAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use crate::fabric::Fut;

/// One instance as the scaler reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    /// `pending`, `running`, `stopping`, `stopped`, `shutting-down`, `terminated`.
    pub state: String,
    pub private_ip: Option<IpAddr>,
    /// Launch time, unix ms.
    pub launched_ms: i64,
}

/// A failed EC2 call: the AWS error code (e.g. `VcpuLimitExceeded`) and message.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct Ec2Error {
    pub code: String,
    pub message: String,
}

/// The three calls hermit makes.
pub trait Ec2: Send + Sync + 'static {
    /// `RunInstances` with launch template `template` (`$Default`), `MinCount = MaxCount = 1`, the user data (plain
    /// text; encoded here) and `tags` on the instance and its volume.
    fn run_instance<'a>(
        &'a self,
        template: &'a str,
        user_data: &'a str,
        tags: &'a [(String, String)],
    ) -> Fut<'a, Result<Instance, Ec2Error>>;
    /// `TerminateInstances`.
    fn terminate<'a>(&'a self, ids: &'a [String]) -> Fut<'a, Result<(), Ec2Error>>;
    /// `DescribeInstances` filtered by `tags` and the states pending, running, stopping, stopped, shutting-down.
    fn describe<'a>(&'a self, tags: &'a [(String, String)]) -> Fut<'a, Result<Vec<Instance>, Ec2Error>>;
    /// Whether S3 object `key` of `bucket` is there (`HeadObject`): a sandbox image in the hosts' image bucket.
    fn has_object<'a>(&'a self, bucket: &'a str, key: &'a str) -> Fut<'a, Result<bool, Ec2Error>>;
    /// Credentials for objects `keys` of `bucket` and nothing else, for an hour (STS `GetFederationToken` with a
    /// session policy naming them): writing them (`write`) or reading them. A frozen sandbox's two objects travel
    /// with these (jarvis4 docs/PLAN-fabric.md).
    fn object_credentials<'a>(
        &'a self,
        _bucket: &'a str,
        _keys: &'a [String],
        _write: bool,
    ) -> Fut<'a, Result<ObjectCredentials, Ec2Error>> {
        Box::pin(async {
            Err(Ec2Error {
                code: "not_supported".into(),
                message: "no STS here".into(),
            })
        })
    }
    /// Deletes object `key` of `bucket` (a missing one is fine).
    fn delete_object<'a>(&'a self, _bucket: &'a str, _key: &'a str) -> Fut<'a, Result<(), Ec2Error>> {
        Box::pin(async {
            Err(Ec2Error {
                code: "not_supported".into(),
                message: "no S3 here".into(),
            })
        })
    }
}

/// Short-lived credentials ([`Ec2::object_credentials`]).
#[derive(Clone, PartialEq, Eq)]
pub struct ObjectCredentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: String,
}

impl std::fmt::Debug for ObjectCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never the secrets
        f.debug_struct("ObjectCredentials")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

/// The session policy of [`Ec2::object_credentials`]: these objects only, to write (with aborting a multipart
/// upload) or to read.
#[must_use]
pub fn object_policy(bucket: &str, keys: &[String], write: bool) -> String {
    let actions: &[&str] = if write {
        &["s3:PutObject", "s3:AbortMultipartUpload"]
    } else {
        &["s3:GetObject"]
    };
    let resources: Vec<String> = keys.iter().map(|k| format!("arn:aws:s3:::{bucket}/{k}")).collect();
    serde_json::json!({"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Action": actions, "Resource": resources}]})
        .to_string()
}

/// The real client.
#[derive(Debug, Clone)]
pub struct Aws {
    client: aws_sdk_ec2::Client,
    s3: aws_sdk_s3::Client,
    sts: aws_sdk_sts::Client,
}

impl Aws {
    /// A client for `region` with static credentials.
    #[must_use]
    pub fn new(region: &str, access_key_id: &str, secret_access_key: &str) -> Self {
        use aws_sdk_ec2::config::timeout::TimeoutConfig;
        use aws_sdk_ec2::config::{BehaviorVersion, Credentials, Region};
        use aws_smithy_http_client::tls;
        use std::time::Duration;
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
            .build_https();
        let conf = aws_sdk_ec2::Config::builder()
            .behavior_version(BehaviorVersion::latest())
            .region(Region::new(region.to_owned()))
            .credentials_provider(Credentials::new(access_key_id, secret_access_key, None, None, "hermit"))
            .http_client(http)
            // a stalled reply (a slow NAT path, 2026-11-07) must fail like any error, never hold the scaler's pass
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(Duration::from_millis(3100))
                    .operation_attempt_timeout(Duration::from_secs(20))
                    .operation_timeout(Duration::from_secs(60))
                    .build(),
            )
            .build();
        // the same account's image bucket, with the same bounds
        let s3 = aws_sdk_s3::Config::builder()
            .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
            .region(aws_sdk_s3::config::Region::new(region.to_owned()))
            .credentials_provider(aws_sdk_s3::config::Credentials::new(
                access_key_id,
                secret_access_key,
                None,
                None,
                "hermit",
            ))
            .http_client(
                aws_smithy_http_client::Builder::new()
                    .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
                    .build_https(),
            )
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(Duration::from_millis(3100))
                    .operation_attempt_timeout(Duration::from_secs(20))
                    .operation_timeout(Duration::from_secs(60))
                    .build(),
            )
            .build();
        // the same user's federation tokens (a frozen sandbox's objects), with the same bounds
        let sts = aws_sdk_sts::Config::builder()
            .behavior_version(aws_sdk_sts::config::BehaviorVersion::latest())
            .region(aws_sdk_sts::config::Region::new(region.to_owned()))
            .credentials_provider(aws_sdk_sts::config::Credentials::new(
                access_key_id,
                secret_access_key,
                None,
                None,
                "hermit",
            ))
            .http_client(
                aws_smithy_http_client::Builder::new()
                    .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
                    .build_https(),
            )
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(Duration::from_millis(3100))
                    .operation_attempt_timeout(Duration::from_secs(20))
                    .operation_timeout(Duration::from_secs(60))
                    .build(),
            )
            .build();
        Self {
            client: aws_sdk_ec2::Client::from_conf(conf),
            s3: aws_sdk_s3::Client::from_conf(s3),
            sts: aws_sdk_sts::Client::from_conf(sts),
        }
    }
}

fn sdk_error<E, R>(e: &aws_sdk_ec2::error::SdkError<E, R>) -> Ec2Error
where
    E: aws_sdk_ec2::error::ProvideErrorMetadata + std::error::Error + 'static,
    R: std::fmt::Debug,
{
    use aws_sdk_ec2::error::ProvideErrorMetadata as _;
    Ec2Error {
        code: e.code().unwrap_or("unknown").to_owned(),
        message: e
            .message()
            .map_or_else(|| aws_sdk_ec2::error::DisplayErrorContext(e).to_string(), str::to_owned),
    }
}

fn instance(i: &aws_sdk_ec2::types::Instance) -> Instance {
    Instance {
        id: i.instance_id().unwrap_or_default().to_owned(),
        state: i
            .state()
            .and_then(|s| s.name())
            .map(|n| n.as_str().to_owned())
            .unwrap_or_default(),
        private_ip: i.private_ip_address().and_then(|a| a.parse().ok()),
        launched_ms: i.launch_time().map_or(0, |t| t.to_millis().unwrap_or(0)),
    }
}

impl Ec2 for Aws {
    fn run_instance<'a>(
        &'a self,
        template: &'a str,
        user_data: &'a str,
        tags: &'a [(String, String)],
    ) -> Fut<'a, Result<Instance, Ec2Error>> {
        use aws_sdk_ec2::types::{LaunchTemplateSpecification, ResourceType, Tag, TagSpecification};
        Box::pin(async move {
            let tagged = |rt: ResourceType| {
                tags.iter()
                    .fold(TagSpecification::builder().resource_type(rt), |b, (k, v)| {
                        b.tags(Tag::builder().key(k).value(v).build())
                    })
                    .build()
            };
            let out = self
                .client
                .run_instances()
                .launch_template(
                    LaunchTemplateSpecification::builder()
                        .launch_template_name(template)
                        .version("$Default")
                        .build(),
                )
                .min_count(1)
                .max_count(1)
                .user_data(STANDARD.encode(user_data))
                .tag_specifications(tagged(ResourceType::Instance))
                .tag_specifications(tagged(ResourceType::Volume))
                .send()
                .await
                .map_err(|e| sdk_error(&e))?;
            out.instances()
                .first()
                .map(instance)
                .filter(|i| !i.id.is_empty())
                .ok_or_else(|| Ec2Error {
                    code: "NoInstance".into(),
                    message: "RunInstances answered no instance".into(),
                })
        })
    }

    fn terminate<'a>(&'a self, ids: &'a [String]) -> Fut<'a, Result<(), Ec2Error>> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(());
            }
            self.client
                .terminate_instances()
                .set_instance_ids(Some(ids.to_vec()))
                .send()
                .await
                .map_err(|e| sdk_error(&e))?;
            Ok(())
        })
    }

    fn describe<'a>(&'a self, tags: &'a [(String, String)]) -> Fut<'a, Result<Vec<Instance>, Ec2Error>> {
        use aws_sdk_ec2::types::Filter;
        Box::pin(async move {
            let mut filters: Vec<Filter> = tags
                .iter()
                .map(|(k, v)| Filter::builder().name(format!("tag:{k}")).values(v).build())
                .collect();
            filters.push(
                ["pending", "running", "stopping", "stopped", "shutting-down"]
                    .iter()
                    .fold(Filter::builder().name("instance-state-name"), |b, s| b.values(*s))
                    .build(),
            );
            let mut out = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let page = self
                    .client
                    .describe_instances()
                    .set_filters(Some(filters.clone()))
                    .set_next_token(token.take())
                    .send()
                    .await
                    .map_err(|e| sdk_error(&e))?;
                for r in page.reservations() {
                    out.extend(r.instances().iter().map(instance));
                }
                match page.next_token() {
                    Some(t) if !t.is_empty() => token = Some(t.to_owned()),
                    _ => break,
                }
            }
            Ok(out)
        })
    }

    fn has_object<'a>(&'a self, bucket: &'a str, key: &'a str) -> Fut<'a, Result<bool, Ec2Error>> {
        use aws_sdk_s3::error::ProvideErrorMetadata as _;
        Box::pin(async move {
            match self.s3.head_object().bucket(bucket).key(key).send().await {
                Ok(_) => Ok(true),
                // a HEAD has no body: a missing key is the modelled NotFound (with s3:ListBucket; 403 without it)
                Err(e)
                    if e.as_service_error()
                        .is_some_and(aws_sdk_s3::operation::head_object::HeadObjectError::is_not_found) =>
                {
                    Ok(false)
                }
                Err(e) => Err(Ec2Error {
                    code: e.code().unwrap_or("unknown").to_owned(),
                    message: e
                        .message()
                        .map_or_else(|| aws_sdk_s3::error::DisplayErrorContext(&e).to_string(), str::to_owned),
                }),
            }
        })
    }

    fn object_credentials<'a>(
        &'a self,
        bucket: &'a str,
        keys: &'a [String],
        write: bool,
    ) -> Fut<'a, Result<ObjectCredentials, Ec2Error>> {
        use aws_sdk_sts::error::ProvideErrorMetadata as _;
        Box::pin(async move {
            let r = self
                .sts
                .get_federation_token()
                .name("hermit-freeze")
                .policy(object_policy(bucket, keys, write))
                .duration_seconds(3600)
                .send()
                .await
                .map_err(|e| Ec2Error {
                    code: e.code().unwrap_or("unknown").to_owned(),
                    message: e.message().map_or_else(
                        || aws_sdk_sts::error::DisplayErrorContext(&e).to_string(),
                        str::to_owned,
                    ),
                })?;
            let c = r.credentials().ok_or_else(|| Ec2Error {
                code: "no_credentials".into(),
                message: "GetFederationToken answered without credentials".into(),
            })?;
            Ok(ObjectCredentials {
                access_key_id: c.access_key_id().to_owned(),
                secret_access_key: c.secret_access_key().to_owned(),
                session_token: c.session_token().to_owned(),
            })
        })
    }

    fn delete_object<'a>(&'a self, bucket: &'a str, key: &'a str) -> Fut<'a, Result<(), Ec2Error>> {
        use aws_sdk_s3::error::ProvideErrorMetadata as _;
        Box::pin(async move {
            self.s3
                .delete_object()
                .bucket(bucket)
                .key(key)
                .send()
                .await
                .map(|_| ())
                .map_err(|e| Ec2Error {
                    code: e.code().unwrap_or("unknown").to_owned(),
                    message: e
                        .message()
                        .map_or_else(|| aws_sdk_s3::error::DisplayErrorContext(&e).to_string(), str::to_owned),
                })
        })
    }
}

#[cfg(test)]
mod object_policy_tests {
    use super::*;

    #[test]
    fn a_freezes_credentials_name_its_two_objects_and_nothing_else() {
        let keys = vec!["frozen/s1/f1".to_owned(), "frozen/s1/f1.scratch".to_owned()];
        let w: serde_json::Value = serde_json::from_str(&object_policy("b", &keys, true)).unwrap();
        assert_eq!(
            w["Statement"][0]["Action"],
            serde_json::json!(["s3:PutObject", "s3:AbortMultipartUpload"])
        );
        assert_eq!(
            w["Statement"][0]["Resource"],
            serde_json::json!(["arn:aws:s3:::b/frozen/s1/f1", "arn:aws:s3:::b/frozen/s1/f1.scratch"])
        );
        let r: serde_json::Value = serde_json::from_str(&object_policy("b", &keys, false)).unwrap();
        assert_eq!(r["Statement"][0]["Action"], serde_json::json!(["s3:GetObject"]));
    }
}

/// A `WireGuard` key pair as `wg genkey | wg pubkey` prints it (base64 of 32 bytes; the private key clamped).
#[derive(Clone)]
pub struct WgKeys {
    pub private_key: String,
    pub public_key: String,
}

impl std::fmt::Debug for WgKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WgKeys")
            .field("public_key", &self.public_key)
            .finish_non_exhaustive()
    }
}

/// A fresh x25519 key pair.
///
/// # Errors
/// When the system's random source fails.
pub fn wg_keys() -> Result<WgKeys, ring::error::Unspecified> {
    use ring::rand::SecureRandom as _;
    let mut sk = [0u8; 32];
    ring::rand::SystemRandom::new().fill(&mut sk)?;
    sk[0] &= 0b1111_1000;
    sk[31] &= 0b0111_1111;
    sk[31] |= 0b0100_0000;
    let pk = x25519_dalek::x25519(sk, x25519_dalek::X25519_BASEPOINT_BYTES);
    Ok(WgKeys {
        private_key: STANDARD.encode(sk),
        public_key: STANDARD.encode(pk),
    })
}

/// The name of a new AWS host: `aws-<6 hex>`.
///
/// # Errors
/// When the system's random source fails.
pub fn host_name() -> Result<String, ring::error::Unspecified> {
    use ring::rand::SecureRandom as _;
    let mut b = [0u8; 3];
    ring::rand::SystemRandom::new().fill(&mut b)?;
    Ok(format!("aws-{:02x}{:02x}{:02x}", b[0], b[1], b[2]))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn wireguard_keys_have_wg_s_shape_and_match() {
        let k = wg_keys().unwrap();
        for s in [&k.private_key, &k.public_key] {
            assert_eq!(s.len(), 44);
            assert!(s.ends_with('='));
        }
        let sk: [u8; 32] = STANDARD.decode(&k.private_key).unwrap().try_into().unwrap();
        assert_eq!(sk[0] & 7, 0, "clamped");
        let pk = x25519_dalek::x25519(sk, x25519_dalek::X25519_BASEPOINT_BYTES);
        assert_eq!(STANDARD.encode(pk), k.public_key);
        // RFC 7748 6.1's test vector: the clamping and the scalar multiplication are x25519's
        let alice: [u8; 32] = (0..32)
            .map(|i| {
                u8::from_str_radix(
                    &"77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a"[2 * i..2 * i + 2],
                    16,
                )
                .unwrap()
            })
            .collect::<Vec<_>>()
            .try_into()
            .unwrap();
        let public = x25519_dalek::x25519(alice, x25519_dalek::X25519_BASEPOINT_BYTES);
        let hex: String = public.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(hex, "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        assert!(host_name().unwrap().starts_with("aws-"));
        assert_eq!(host_name().unwrap().len(), 10);
    }
}
