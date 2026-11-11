//! Telegram: the account (`MTProto`, grammers) creates channels; the bot (Bot API over HTTPS) posts and reads them.
//! Both leave through the cluster's SOCKS5 way out (`HERALD_SOCKS`).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use grammers_client::sender::ConnectionParams;
use grammers_client::tl;
use grammers_client::{Client, InvocationError, SenderPool};
use serde::Deserialize;
use serde_json::json;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A channel herald made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// The Bot API chat id (`-100…`).
    pub chat_id: i64,
    pub invite_link: String,
}

/// What a channel looks like to the bot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub title: String,
    pub invite_link: Option<String>,
}

/// How a message's text is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Html,
}

/// A failed Telegram call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Telegram asks to wait this many seconds.
    #[error("Telegram asks to wait {0} s")]
    FloodWait(u64),
    /// Telegram refused the request itself (the text, the title …); the message says why.
    #[error("Telegram refused: {0}")]
    Refused(String),
    /// The channel is gone or the bot is no longer in it.
    #[error("no such channel")]
    NoChannel,
    /// Telegram could not be reached or failed.
    #[error("Telegram unavailable: {0}")]
    Unavailable(String),
}

/// What herald needs from Telegram (faked in tests).
pub trait Telegram: Send + Sync {
    fn create_channel<'a>(&'a self, title: &'a str, about: &'a str) -> BoxFuture<'a, Result<Created, Error>>;
    fn channel(&self, chat_id: i64) -> BoxFuture<'_, Result<Info, Error>>;
    fn post<'a>(
        &'a self,
        chat_id: i64,
        text: &'a str,
        format: Format,
        silent: bool,
    ) -> BoxFuture<'a, Result<i64, Error>>;
}

/// The live connection. The account is connected only while it creates a channel, one creation at a time per
/// replica, and disconnected after: two replicas (a rolling update) never hold its key on lasting connections at
/// once, which Telegram may answer by ending the session. The bot is plain HTTPS.
pub struct Live {
    cfg: Config,
    bot: Bot,
    bot_user: tl::enums::InputUser,
    account: tokio::sync::Mutex<()>,
}

/// The account, connected for one piece of work.
struct Account {
    client: Client,
    runner: tokio::task::JoinHandle<()>,
}

impl Drop for Account {
    fn drop(&mut self) {
        self.client.disconnect();
        self.runner.abort();
    }
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live").finish_non_exhaustive()
    }
}

/// How herald reaches Telegram.
#[derive(Debug, Clone)]
pub struct Config {
    pub api_id: i32,
    /// The account's stored session (`session::load`).
    pub session: String,
    pub bot_token: String,
    /// `socks5://host:port`, the cluster's way out.
    pub socks: String,
    pub version: String,
}

/// A failed start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("{0}")]
    Session(#[from] crate::session::Error),
    #[error("the Telegram session is not signed in (run `herald login` again)")]
    NotSignedIn,
    #[error("Telegram: {0}")]
    Telegram(String),
}

/// The account's connection parameters: herald names itself, and every connection goes through `socks`.
#[must_use]
pub fn connection_params(socks: &str, version: &str) -> ConnectionParams {
    ConnectionParams {
        device_model: "herald".to_owned(),
        system_version: "Linux".to_owned(),
        app_version: version.to_owned(),
        system_lang_code: "en".to_owned(),
        lang_code: "en".to_owned(),
        proxy_url: Some(socks.to_owned()),
        ..ConnectionParams::default()
    }
}

impl Live {
    /// Checks the account is signed in and looks the bot up as the account sees it, then disconnects the account.
    ///
    /// # Errors
    /// When the session is unusable or signed out, or Telegram can't be reached.
    pub async fn start(cfg: &Config) -> Result<Self, StartError> {
        let account = connect(cfg)?;
        match account.client.is_authorized().await {
            Ok(true) => {}
            Ok(false) => return Err(StartError::NotSignedIn),
            Err(e) => return Err(StartError::Telegram(e.to_string())),
        }
        let bot =
            Bot::new(&cfg.bot_token, &cfg.socks, &cfg.version).map_err(|e| StartError::Telegram(e.to_string()))?;
        let username = bot.username().await.map_err(|e| StartError::Telegram(e.to_string()))?;
        let resolved = account
            .client
            .invoke(&tl::functions::contacts::ResolveUsername {
                username: username.clone(),
                referer: None,
            })
            .await
            .map_err(|e| StartError::Telegram(format!("resolve @{username}: {e}")))?;
        drop(account);
        let tl::enums::contacts::ResolvedPeer::Peer(peer) = resolved;
        let bot_user = peer
            .users
            .into_iter()
            .find_map(|u| match u {
                tl::enums::User::User(u) if u.bot => u.access_hash.map(|access_hash| {
                    tl::enums::InputUser::User(tl::types::InputUser {
                        user_id: u.id,
                        access_hash,
                    })
                }),
                _ => None,
            })
            .ok_or_else(|| StartError::Telegram(format!("@{username} did not resolve to a bot")))?;
        tracing::info!(bot = %username, "telegram checked");
        Ok(Self {
            cfg: cfg.clone(),
            bot,
            bot_user,
            account: tokio::sync::Mutex::new(()),
        })
    }
}

/// The account on a fresh connection from the stored session.
fn connect(cfg: &Config) -> Result<Account, crate::session::Error> {
    let session = Arc::new(crate::session::load(&cfg.session)?);
    let pool = SenderPool::with_configuration(session, cfg.api_id, connection_params(&cfg.socks, &cfg.version));
    let client = Client::new(pool.handle);
    let runner = tokio::spawn(pool.runner.run());
    Ok(Account { client, runner })
}

fn from_invocation(e: InvocationError) -> Error {
    match e {
        InvocationError::Rpc(r)
            if r.name == "FLOOD_WAIT" || r.name == "FLOOD_PREMIUM_WAIT" || r.name == "SLOWMODE_WAIT" =>
        {
            Error::FloodWait(u64::from(r.value.unwrap_or(30)))
        }
        InvocationError::Rpc(r) if r.code == 400 => Error::Refused(r.name),
        other => Error::Unavailable(other.to_string()),
    }
}

impl Telegram for Live {
    fn create_channel<'a>(&'a self, title: &'a str, about: &'a str) -> BoxFuture<'a, Result<Created, Error>> {
        Box::pin(async move {
            let _one = self.account.lock().await;
            let account = connect(&self.cfg).map_err(|e| Error::Unavailable(e.to_string()))?;
            let updates = account
                .client
                .invoke(&tl::functions::channels::CreateChannel {
                    broadcast: true,
                    megagroup: false,
                    for_import: false,
                    forum: false,
                    title: title.to_owned(),
                    about: about.to_owned(),
                    geo_point: None,
                    address: None,
                    ttl_period: None,
                })
                .await
                .map_err(from_invocation)?;
            let chats = match updates {
                tl::enums::Updates::Updates(u) => u.chats,
                tl::enums::Updates::Combined(u) => u.chats,
                _ => Vec::new(),
            };
            let (id, access_hash) = chats
                .into_iter()
                .find_map(|c| match c {
                    tl::enums::Chat::Channel(c) => c.access_hash.map(|h| (c.id, h)),
                    _ => None,
                })
                .ok_or_else(|| Error::Unavailable("Telegram answered the create without the channel".to_owned()))?;
            let channel = tl::enums::InputChannel::Channel(tl::types::InputChannel {
                channel_id: id,
                access_hash,
            });
            account
                .client
                .invoke(&tl::functions::channels::EditAdmin {
                    channel: channel.clone(),
                    user_id: self.bot_user.clone(),
                    admin_rights: tl::enums::ChatAdminRights::Rights(tl::types::ChatAdminRights {
                        change_info: false,
                        post_messages: true,
                        edit_messages: true,
                        delete_messages: true,
                        ban_users: false,
                        invite_users: true,
                        pin_messages: true,
                        add_admins: false,
                        anonymous: false,
                        manage_call: false,
                        other: false,
                        manage_topics: false,
                        post_stories: false,
                        edit_stories: false,
                        delete_stories: false,
                        manage_direct_messages: false,
                        manage_ranks: false,
                        manage_linked_peers: false,
                        manage_welcome_messages: false,
                    }),
                    rank: None,
                })
                .await
                .map_err(from_invocation)?;
            let invite = account
                .client
                .invoke(&tl::functions::messages::ExportChatInvite {
                    legacy_revoke_permanent: false,
                    request_needed: false,
                    peer: tl::enums::InputPeer::Channel(tl::types::InputPeerChannel {
                        channel_id: id,
                        access_hash,
                    }),
                    expire_date: None,
                    usage_limit: None,
                    title: None,
                    subscription_pricing: None,
                })
                .await
                .map_err(from_invocation)?;
            let invite_link = match invite {
                tl::enums::ExportedChatInvite::ChatInviteExported(i) => i.link,
                tl::enums::ExportedChatInvite::ChatInvitePublicJoinRequests => {
                    return Err(Error::Unavailable("Telegram answered no invite link".to_owned()));
                }
            };
            Ok(Created {
                chat_id: bot_chat_id(id),
                invite_link,
            })
        })
    }

    fn channel(&self, chat_id: i64) -> BoxFuture<'_, Result<Info, Error>> {
        Box::pin(self.bot.get_chat(chat_id))
    }

    fn post<'a>(
        &'a self,
        chat_id: i64,
        text: &'a str,
        format: Format,
        silent: bool,
    ) -> BoxFuture<'a, Result<i64, Error>> {
        Box::pin(self.bot.send_message(chat_id, text, format, silent))
    }
}

/// The Bot API's id of `MTProto` channel `id`: `-100` followed by the id.
#[must_use]
pub fn bot_chat_id(id: i64) -> i64 {
    -1_000_000_000_000 - id
}

/// The Bot API client.
struct Bot {
    http: reqwest::Client,
    base: String,
}

#[derive(Deserialize)]
struct Answer<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
    error_code: Option<u16>,
    parameters: Option<Parameters>,
}

#[derive(Deserialize)]
struct Parameters {
    retry_after: Option<u64>,
}

impl Bot {
    fn new(token: &str, socks: &str, version: &str) -> Result<Self, reqwest::Error> {
        // socks5h: the proxy resolves api.telegram.org (the pod never looks up an outside name)
        let proxy = reqwest::Proxy::all(socks.replacen("socks5://", "socks5h://", 1))?;
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(aviary_core::http::tls_config())
            .user_agent(format!("herald/{version}"))
            .proxy(proxy)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base: format!("https://api.telegram.org/bot{token}"),
        })
    }

    async fn call<T: for<'de> Deserialize<'de>>(&self, method: &str, body: serde_json::Value) -> Result<T, Error> {
        let res = self
            .http
            .post(format!("{}/{method}", self.base))
            .json(&body)
            .send()
            .await
            // never the URL: it holds the token
            .map_err(|e| Error::Unavailable(format!("{method}: {}", e.without_url())))?;
        let answer: Answer<T> = res
            .json()
            .await
            .map_err(|e| Error::Unavailable(format!("{method}: {}", e.without_url())))?;
        if answer.ok
            && let Some(r) = answer.result
        {
            return Ok(r);
        }
        let description = answer.description.unwrap_or_default();
        Err(
            match (answer.error_code, answer.parameters.and_then(|p| p.retry_after)) {
                (_, Some(secs)) => Error::FloodWait(secs),
                (Some(400 | 403), _)
                    if description.contains("chat not found")
                        || description.contains("not a member")
                        || description.contains("kicked") =>
                {
                    Error::NoChannel
                }
                (Some(400), _) => Error::Refused(description),
                _ => Error::Unavailable(format!("{method}: {description}")),
            },
        )
    }

    async fn username(&self) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Me {
            username: String,
        }
        let me: Me = self.call("getMe", json!({})).await?;
        Ok(me.username)
    }

    async fn get_chat(&self, chat_id: i64) -> Result<Info, Error> {
        #[derive(Deserialize)]
        struct Chat {
            title: Option<String>,
            invite_link: Option<String>,
        }
        let c: Chat = self.call("getChat", json!({ "chat_id": chat_id })).await?;
        Ok(Info {
            title: c.title.unwrap_or_default(),
            invite_link: c.invite_link,
        })
    }

    async fn send_message(&self, chat_id: i64, text: &str, format: Format, silent: bool) -> Result<i64, Error> {
        #[derive(Deserialize)]
        struct Sent {
            message_id: i64,
        }
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "disable_notification": silent,
            "link_preview_options": { "is_disabled": true },
        });
        if format == Format::Html {
            body["parse_mode"] = json!("HTML");
        }
        let sent: Sent = self.call("sendMessage", body).await?;
        Ok(sent.message_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bot_chat_ids_carry_the_minus_100_prefix() {
        assert_eq!(bot_chat_id(1_234_567_890), -1_001_234_567_890);
        assert_eq!(bot_chat_id(42).to_string(), "-1000000000042");
    }
}
