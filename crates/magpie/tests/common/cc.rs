//! A test supervisor for the cc routes: the gateway's token and sandbox identity headers, the lease, and one call per
//! FUSE operation with a fresh `opId`.

use std::time::Duration;

use magpie_api::client::{Client, ClientError, SandboxCalls};
use magpie_api::conversations::CreateRequest;
use magpie_api::sandbox::{
    CcFileQuery, CcMkdir, CcPathOp, CcRename, CcSetattr, CcSizeVersion, CcTree, CcTruncate, CcVersion, CcWriteQuery,
    LeaseResponse,
};
use magpie_api::turns::SendRequest;
use reqwest::header::{HeaderMap, HeaderValue};
use uuid::Uuid;

use super::Replica;

/// A conversation of `person`, created through pigeon's route.
pub async fn conversation(r: &Replica, person: &str) -> Uuid {
    let key = Uuid::new_v4().to_string();
    r.pigeon()
        .person(person)
        .create_conversation(&CreateRequest {
            idempotency_key: key.clone(),
            message: SendRequest {
                client_message_id: format!("{key}:m"),
                text: String::new(),
                images: Vec::new(),
                files: Vec::new(),
            },
        })
        .await
        .unwrap()
        .get()
        .conversation
        .id
}

/// Records `sandbox` as hermit's live sandbox of `conversation` (what the feed follower writes).
pub async fn hermit_names(pool: &aviary_pg::Pool, conversation: Uuid, sandbox: Option<Uuid>, state: &str) {
    pool.get()
        .await
        .unwrap()
        .execute(
            "update conversations set hermit_sandbox = $2, hermit_state = $3 where id = $1",
            &[&conversation, &sandbox, &state],
        )
        .await
        .unwrap();
}

/// Sets the main transcript path directly, with the provider session id it names (tests that do not exercise the
/// `running` report).
pub async fn set_main(pool: &aviary_pg::Pool, conversation: Uuid, path: &str) {
    try_set_main(pool, conversation, path).await.unwrap();
}

/// [`set_main`], its error returned.
pub async fn try_set_main(pool: &aviary_pg::Pool, conversation: Uuid, path: &str) -> magpie::error::Result<()> {
    let sid = path
        .strip_prefix("projects/-workspace/")
        .and_then(|p| p.strip_suffix(".jsonl"))
        .unwrap_or(path);
    aviary_pg::tx!(pool, |t| -> magpie::error::Result<()> {
        t.execute(
            "update conversations set provider_session_id = $2 where id = $1",
            &[&conversation, &sid],
        )
        .await?;
        magpie::ccfs::lines::set_main_transcript(t, conversation, path).await
    })
    .await
}

/// Starts a person-origin turn (its `user.sent` echo not matched yet) as the conversation's active turn, the earlier
/// ones completed; `s`'s writes name it.
pub async fn person_turn(pool: &aviary_pg::Pool, s: &mut Sandbox) -> Uuid {
    let id = Uuid::new_v4();
    let c = pool.get().await.unwrap();
    c.execute(
        "update turns set state = 'completed' where conversation_id = $1",
        &[&s.conversation],
    )
    .await
    .unwrap();
    c.execute(
        "insert into turns (id, conversation_id, origin, state) values ($1, $2, 'person', 'running')",
        &[&id, &s.conversation],
    )
    .await
    .unwrap();
    c.execute(
        "update conversations set active_turn = $2 where id = $1",
        &[&s.conversation, &id],
    )
    .await
    .unwrap();
    s.turn = Some(id);
    id
}

/// The end report of `turn` (`None`: operations credited to no turn): its limbo flushed; how many lines were
/// retracted.
pub async fn flush_turn(pool: &aviary_pg::Pool, conv: Uuid, turn: Option<Uuid>) -> usize {
    aviary_pg::tx!(pool, |t| -> magpie::error::Result<usize> {
        t.execute("select 1 from conversations where id = $1 for update", &[&conv])
            .await?;
        magpie::ccfs::limbo::flush_scope(t, conv, turn).await
    })
    .await
    .unwrap()
}

/// One sandbox's supervisor.
pub struct Sandbox {
    pub client: Client,
    pub lease: Option<Uuid>,
    pub conversation: Uuid,
    pub id: Uuid,
    pub turn: Option<Uuid>,
}

pub const SID: &str = "11111111-2222-4333-8444-555555555555";

/// The main transcript path of [`SID`].
pub fn main_path() -> String {
    format!("projects/-workspace/{SID}.jsonl")
}

impl Sandbox {
    pub fn new(r: &Replica, conversation: Uuid, id: Uuid, person: &str) -> Self {
        let mut h = HeaderMap::new();
        h.insert("aviary-sandbox-id", HeaderValue::from_str(&id.to_string()).unwrap());
        h.insert(
            "aviary-sandbox-conversation",
            HeaderValue::from_str(&conversation.to_string()).unwrap(),
        );
        h.insert("aviary-sandbox-person", HeaderValue::from_str(person).unwrap());
        let build = aviary_core::version::Build::new("magpie-test");
        let http = aviary_core::http::client_builder(&build, None)
            .default_headers(h)
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap();
        Self {
            client: Client::new(http, r.url.clone(), r.tokens.gateway.clone()),
            lease: None,
            conversation,
            id,
            turn: None,
        }
    }

    /// A conversation of `person` with this sandbox named live by hermit, and the lease taken.
    pub async fn leased(r: &Replica, person: &str) -> Self {
        let conversation = conversation(r, person).await;
        let id = Uuid::new_v4();
        hermit_names(r.pool(), conversation, Some(id), "ready").await;
        let mut s = Self::new(r, conversation, id, person);
        s.take_lease().await.unwrap();
        s
    }

    pub fn calls(&self) -> SandboxCalls<'_> {
        self.client.sandbox(self.lease)
    }

    pub async fn take_lease(&mut self) -> Result<LeaseResponse, ClientError> {
        let l = self.calls().lease().await?;
        self.lease = Some(l.lease_id);
        Ok(l)
    }

    pub fn wq(&self, path: &str, offset: u64, op_id: Uuid) -> CcWriteQuery {
        CcWriteQuery {
            path: path.to_owned(),
            offset,
            mtime_ns: 1,
            op_id,
            create: true,
            mode: 0o600,
            turn_id: self.turn,
            append: false,
        }
    }

    pub async fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<CcSizeVersion, ClientError> {
        self.calls()
            .cc_write(&self.wq(path, offset, Uuid::new_v4()), data.to_vec())
            .await
    }

    pub async fn read(&self, path: &str) -> Result<Vec<u8>, ClientError> {
        let res = self
            .calls()
            .cc_file(&CcFileQuery {
                path: path.to_owned(),
                offset: None,
                length: None,
            })
            .await?;
        Ok(res.bytes().await.unwrap().to_vec())
    }

    pub async fn read_range(&self, path: &str, offset: u64, length: u64) -> Result<(u16, Vec<u8>), ClientError> {
        let res = self
            .calls()
            .cc_file(&CcFileQuery {
                path: path.to_owned(),
                offset: Some(offset),
                length: Some(length),
            })
            .await?;
        let status = res.status().as_u16();
        Ok((status, res.bytes().await.unwrap().to_vec()))
    }

    pub async fn truncate(&self, path: &str, size: u64) -> Result<CcSizeVersion, ClientError> {
        self.calls()
            .cc_truncate(&CcTruncate {
                path: path.to_owned(),
                size,
                mtime_ns: 2,
                op_id: Uuid::new_v4(),
            })
            .await
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<CcVersion, ClientError> {
        self.calls()
            .cc_rename(&CcRename {
                from: from.to_owned(),
                to: to.to_owned(),
                op_id: Uuid::new_v4(),
            })
            .await
    }

    pub async fn unlink(&self, path: &str) -> Result<(), ClientError> {
        self.calls()
            .cc_unlink(&CcPathOp {
                path: path.to_owned(),
                op_id: Uuid::new_v4(),
            })
            .await
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), ClientError> {
        self.calls()
            .cc_mkdir(&CcMkdir {
                path: path.to_owned(),
                mode: 0o700,
                mtime_ns: 3,
                op_id: Uuid::new_v4(),
            })
            .await
    }

    /// `mkdir -p`.
    pub async fn mkdirs(&self, path: &str) {
        let mut at = String::new();
        for part in path.split('/') {
            if !at.is_empty() {
                at.push('/');
            }
            at.push_str(part);
            match self.mkdir(&at).await {
                Ok(()) => {}
                Err(e) if e.code().is_some_and(|c| c.as_str() == "exists") => {}
                Err(e) => panic!("mkdir {at}: {e:?}"),
            }
        }
    }

    pub async fn rmdir(&self, path: &str) -> Result<(), ClientError> {
        self.calls()
            .cc_rmdir(&CcPathOp {
                path: path.to_owned(),
                op_id: Uuid::new_v4(),
            })
            .await
    }

    pub async fn setattr(
        &self,
        path: &str,
        mode: Option<u32>,
        mtime_ns: Option<i64>,
    ) -> Result<CcVersion, ClientError> {
        self.calls()
            .cc_setattr(&CcSetattr {
                path: path.to_owned(),
                mode,
                mtime_ns,
                op_id: Uuid::new_v4(),
            })
            .await
    }

    pub async fn tree(&self) -> Result<CcTree, ClientError> {
        self.calls().cc_tree().await
    }
}

/// The code of an API error.
pub fn code(e: &ClientError) -> String {
    e.code()
        .map_or_else(|| panic!("not an API error: {e:?}"), |c| c.as_str().to_owned())
}
