//! The legacy-path set (DESIGN 9.5, cutover 10.7), until A7: old asset paths resolve through `uploads.legacy_path`
//! (the tee's record) and `import_uploads` (the importer's ingestion) any number of times with a fresh copy per send,
//! outlive retention and the purge of a conversation the upload was attached to, and a miss is `unknown_upload`.

#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::many_single_char_names
)]

mod common;

use magpie::legacy::paths;
use magpie_api::errors::ErrorCode;
use magpie_api::legacy::LegacyPathRequest;
use magpie_api::turns::{FileRef, SendRequest};
use magpie_api::uploads::{UploadKind, UploadResponse};
use uuid::Uuid;

const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";

fn key(person: &str) -> String {
    aviary_core::log::person_key(person)
}

/// An old asset path of `person` for the file `name`, as the old child returned it.
fn old_path(person: &str, name: &str) -> String {
    format!("/home/{}/.claude/cloudcli-assets/{name}", key(person))
}

fn send(cmid: &str, files: Vec<FileRef>) -> SendRequest {
    SendRequest {
        client_message_id: cmid.into(),
        text: "整理一下".into(),
        images: Vec::new(),
        files,
    }
}

fn file(path: &str, up: &UploadResponse) -> FileRef {
    FileRef {
        path: path.to_owned(),
        name: up.name.clone(),
        mime_type: up.mime_type.clone(),
        size: up.size,
    }
}

async fn upload(r: &common::Replica, person: &str, name: &str, bytes: &[u8]) -> UploadResponse {
    r.pigeon()
        .person(person)
        .upload(name, UploadKind::File, "application/pdf", bytes.to_vec())
        .await
        .unwrap()
}

/// What the importer's ingestion left (until A6): `name` in the old asset folder of [`ALICE`] is `upload`.
async fn import_upload(r: &common::Replica, name: &str, upload: Uuid) {
    r.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "insert into import_uploads (person, legacy_path, upload_id, sha256) values ($1, $2, $3, $4)",
            &[&ALICE, &name, &upload, &vec![1u8; 32]],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_paths_resolve_through_both_tables_any_number_of_times() {
    let db = common::fresh_db("lg_paths").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    // the tee: an upload recorded under the old path the old child returned
    let teed = upload(&r, ALICE, "teed.pdf", b"%PDF-1.4 teed").await;
    a.upload_legacy_path(
        teed.upload_id,
        &LegacyPathRequest {
            legacy_path: old_path(ALICE, "1762054010120-teed.pdf"),
        },
    )
    .await
    .unwrap();
    // a path that is not the person's: 400 bad_path; another person's upload: 404
    let bad = a
        .upload_legacy_path(
            teed.upload_id,
            &LegacyPathRequest {
                legacy_path: old_path(BOB, "x.pdf"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(bad.code(), Some(&ErrorCode::BadPath));
    let bobs = upload(&r, BOB, "b.pdf", b"bob").await;
    let nf = a
        .upload_legacy_path(
            bobs.upload_id,
            &LegacyPathRequest {
                legacy_path: old_path(ALICE, "b.pdf"),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(nf.code(), Some(&ErrorCode::NotFound));
    // the importer's ingestion: an unattached upload under import_uploads
    let imported = upload(&r, ALICE, "imported.pdf", b"%PDF-1.4 imported").await;
    import_upload(&r, "1761000000000-imported.pdf", imported.upload_id).await;
    // each old path resolves in several conversations, and twice in one, each send attaching a fresh copy
    let c1 = common::files::conversation(&r, ALICE, "p1").await;
    let c2 = common::files::conversation(&r, ALICE, "p2").await;
    let teed_path = old_path(ALICE, "1762054010120-teed.pdf");
    let imp_path = format!(".claude/cloudcli-assets/{}", "1761000000000-imported.pdf");
    let mut attached = Vec::new();
    for (i, c) in [c1, c2].into_iter().enumerate() {
        a.send_turn(
            c,
            &send(
                &format!("p:{i}"),
                vec![file(&teed_path, &teed), file(&imp_path, &imported)],
            ),
        )
        .await
        .unwrap();
        // end the turn so a second send is admitted
        r.pool()
            .get()
            .await
            .unwrap()
            .execute(
                "update conversations set run_state = 'idle', active_turn = null where id = $1",
                &[&c],
            )
            .await
            .unwrap();
    }
    a.send_turn(c1, &send("p:again", vec![file(&teed_path, &teed)]))
        .await
        .unwrap();
    let conn = r.pool().get().await.unwrap();
    for row in conn
        .query(
            "select id, conversation_id, name, blob from uploads where conversation_id is not null order by created_at",
            &[],
        )
        .await
        .unwrap()
    {
        attached.push((row.get::<_, Uuid>(0), row.get::<_, Uuid>(1), row.get::<_, String>(2)));
    }
    assert_eq!(attached.len(), 5, "{attached:?}");
    let ids: std::collections::HashSet<Uuid> = attached.iter().map(|x| x.0).collect();
    assert_eq!(ids.len(), 5, "a fresh copy per send");
    assert!(!ids.contains(&teed.upload_id) && !ids.contains(&imported.upload_id));
    // the originals stay unattached, and each copy has its own blob refs
    let unattached: i64 = conn
        .query_one(
            "select count(*) from uploads where id = any($1) and conversation_id is null",
            &[&vec![teed.upload_id, imported.upload_id]],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(unattached, 2);
    let refs: i64 = conn
        .query_one(
            "select count(*) from blob_refs where owner_kind = 'upload' and owner = any($1)",
            &[&ids.iter().copied().collect::<Vec<_>>()],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(refs, 5);
    // a repeated send makes no copy
    let before: i64 = conn
        .query_one("select count(*) from uploads", &[])
        .await
        .unwrap()
        .get(0);
    a.send_turn(c1, &send("p:again", vec![file(&teed_path, &teed)]))
        .await
        .unwrap();
    let after: i64 = conn
        .query_one("select count(*) from uploads", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(before, after);
    // an old path in neither table: unknown_upload (the synchronous ingest went in A6)
    let miss = a
        .send_turn(c2, &send("p:miss", vec![file(&old_path(ALICE, "never.pdf"), &teed)]))
        .await
        .unwrap_err();
    assert_eq!(miss.code(), Some(&ErrorCode::UnknownUpload));
    // another person's old path is not an old path of this person: unknown_upload
    let other = a
        .send_turn(c2, &send("p:other", vec![file(&old_path(BOB, "x.pdf"), &teed)]))
        .await
        .unwrap_err();
    assert_eq!(other.code(), Some(&ErrorCode::UnknownUpload));
    // M3's first message resolves the same way
    let created = a
        .create_conversation(&magpie_api::conversations::CreateRequest {
            model: None,
            thinking: None,
            idempotency_key: "p3".into(),
            message: send("p3:1", vec![file(&teed_path, &teed)]),
        })
        .await
        .unwrap()
        .into_inner();
    assert!(created.turn.is_some());
    // the tee's record is an unattached copy of its own; a repeated PUT adds none
    let teed_record = paths::lookup(&conn, ALICE, "1762054010120-teed.pdf")
        .await
        .unwrap()
        .unwrap();
    assert_ne!(teed_record, teed.upload_id);
    a.upload_legacy_path(
        teed.upload_id,
        &LegacyPathRequest {
            legacy_path: teed_path.clone(),
        },
    )
    .await
    .unwrap();
    let records: i64 = conn
        .query_one(
            "select count(*) from uploads where legacy_path = '1762054010120-teed.pdf'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(records, 1);
    // retention keeps the legacy-path set past the unattached-upload age; a plain unattached upload goes
    let plain = upload(&r, ALICE, "plain.pdf", b"plain").await;
    conn.execute(
        "update uploads set created_at = now() - interval '3 days' where conversation_id is null",
        &[],
    )
    .await
    .unwrap();
    magpie::jobs::retention::pass(r.pool()).await.unwrap();
    let left: Vec<Uuid> = conn
        .query("select id from uploads where conversation_id is null", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert!(left.contains(&teed_record) && left.contains(&imported.upload_id));
    assert!(!left.contains(&plain.upload_id));
    // and they still resolve
    a.send_turn(
        c2,
        &send("p:late", vec![file(&imp_path, &imported), file(&teed_path, &teed)]),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_teed_upload_attached_by_its_id_and_purged_keeps_its_old_path() {
    let db = common::fresh_db("lg_teepurge").await;
    let t = common::Tokens::new();
    let r = common::replica(&db.url, &t).await;
    let a = r.pigeon();
    let a = a.person(ALICE);
    let teed = upload(&r, ALICE, "g.pdf", b"%PDF-1.4 gate").await;
    let path = old_path(ALICE, "1762054020000-g.pdf");
    a.upload_legacy_path(
        teed.upload_id,
        &LegacyPathRequest {
            legacy_path: path.clone(),
        },
    )
    .await
    .unwrap();
    // pigeon attaches the upload itself by its magpie id (the gate-time path), then the conversation is purged
    let c = common::files::conversation(&r, ALICE, "g").await;
    a.send_turn(
        c,
        &send("g:1", vec![file(&format!("uploads/{}/g.pdf", teed.upload_id), &teed)]),
    )
    .await
    .unwrap();
    r.pool()
        .get()
        .await
        .unwrap()
        .execute(
            "update conversations set run_state = 'idle', active_turn = null where id = $1",
            &[&c],
        )
        .await
        .unwrap();
    a.delete_conversation(c).await.unwrap();
    a.purge_conversation(c).await.unwrap();
    let gone: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from uploads where id = $1", &[&teed.upload_id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(gone, 0);
    // a composer still holding the old path sends it elsewhere: it resolves through the record
    let c2 = common::files::conversation(&r, ALICE, "g2").await;
    a.send_turn(c2, &send("g2:1", vec![file(&path, &teed)])).await.unwrap();
    let n: i64 = r
        .pool()
        .get()
        .await
        .unwrap()
        .query_one("select count(*) from uploads where conversation_id = $1", &[&c2])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
}
