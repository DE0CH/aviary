//! `video-check`: whether the model this generation runs watches video and hears sound (Deyao, 2026-11-18). A
//! command (a link to the supervisor binary) a skill runs before video work, so a model that cannot see footage says
//! so in its first reply instead of editing blind.
//!
//! It asks by effect, not from a list: one chat completions call through the model route with a one-second video and
//! a half-second sound (`video_check/probe.*`, 4 KB together, the parts Qwen Code sends) as the generation's model
//! ([`MODEL_ENV`]). An answer is a yes; the route's "No endpoints found that support input video/audio" is a no; any
//! other refusal is "cannot tell". On a no it asks the same of the route's other models and names those that answer.
//!
//! Exit 0: it sees and hears; [`EXIT_BLIND`]: it does not; 1: cannot tell; 2: bad usage.

use std::io::Write as _;

use base64::Engine as _;
use serde_json::{Value, json};

/// The generation's model id, in the harness's environment (`cc::launch::fixed_env`).
pub const MODEL_ENV: &str = "AVIARY_MODEL";
/// The model does not take video or sound.
pub const EXIT_BLIND: i32 = 3;

const PROBE_VIDEO: &[u8] = include_bytes!("video_check/probe.mp4");
const PROBE_SOUND: &[u8] = include_bytes!("video_check/probe.mp3");
const CHAT: &str = "/v1/chat/completions";
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// What one probe found.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Sees,
    /// The route's reason.
    Blind(String),
    /// Why it cannot tell.
    Unknown(String),
}

/// The probe for `model`: the video and the sound as Qwen Code sends them (a `video_url` data URI, `input_audio`'s
/// bare base64), a few tokens at most.
#[must_use]
pub fn probe_body(model: &str) -> Value {
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    json!({
        "model": model,
        "max_tokens": 16,
        "messages": [{"role": "user", "content": [
            {"type": "text", "text": "Name the colour and the sound in one word each."},
            {"type": "video_url", "video_url": {"url": format!("data:video/mp4;base64,{}", b64(PROBE_VIDEO))}},
            {"type": "input_audio", "input_audio": {"data": b64(PROBE_SOUND), "format": "mp3"}},
        ]}],
    })
}

/// The verdict of the route's answer.
#[must_use]
pub fn verdict(status: u16, body: &[u8]) -> Verdict {
    let text = String::from_utf8_lossy(body);
    let message = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| text.chars().take(300).collect());
    if (200..300).contains(&status) {
        Verdict::Sees
    } else if message.contains("support input video") || message.contains("support input audio") {
        Verdict::Blind(message)
    } else {
        Verdict::Unknown(format!("the model route answered {status}: {message}"))
    }
}

/// The text and exit code for `model` (its label `label`), its verdict and the other models that see and hear.
#[must_use]
pub fn report(label: &str, v: &Verdict, others: &[String]) -> (String, i32) {
    match v {
        Verdict::Sees => (
            format!("{label} (this conversation's model) watches video and hears sound."),
            0,
        ),
        Verdict::Blind(why) => {
            let can = if others.is_empty() {
                "No model of this deployment does.".to_owned()
            } else {
                format!("Models that do: {}.", others.join(", "))
            };
            (
                format!(
                    "{label} (this conversation's model) cannot watch video or hear sound ({why}). {can} \
                     The person picks the model in the model menu."
                ),
                EXIT_BLIND,
            )
        }
        Verdict::Unknown(why) => (format!("video-check: cannot tell for {label}: {why}"), 1),
    }
}

async fn probe(http: &reqwest::Client, base: &str, model: &str) -> Verdict {
    let sent = http
        .post(format!("{base}{CHAT}"))
        .timeout(TIMEOUT)
        .json(&probe_body(model))
        .send()
        .await;
    match sent {
        Ok(r) => {
            let status = r.status().as_u16();
            match r.bytes().await {
                Ok(b) => verdict(status, &b),
                Err(e) => Verdict::Unknown(format!("{status}: {e}")),
            }
        }
        Err(e) => Verdict::Unknown(format!("the model route is unreachable: {e}")),
    }
}

/// The check against the model route at `base`.
pub async fn check(http: &reqwest::Client, base: &str, model: &str) -> (String, i32) {
    let models = gateway_api::models::fetch(http, base, None).await.ok();
    let label_of = |id: &str| {
        models
            .as_ref()
            .and_then(|m| m.models.iter().find(|x| x.id == id))
            .map_or_else(|| id.to_owned(), |x| x.label.clone())
    };
    let v = probe(http, base, model).await;
    let mut others = Vec::new();
    if matches!(v, Verdict::Blind(_))
        && let Some(m) = &models
    {
        let rest: Vec<_> = m.models.iter().filter(|x| x.id != model).collect();
        let found = futures_util::future::join_all(rest.iter().map(|x| probe(http, base, &x.id))).await;
        others = rest
            .iter()
            .zip(found)
            .filter(|(_, f)| *f == Verdict::Sees)
            .map(|(x, _)| x.label.clone())
            .collect();
    }
    report(&label_of(model), &v, &others)
}

/// `video-check`: the verdict on stdout (stderr when it cannot tell).
#[must_use]
pub fn cli(args: &[String], build: &aviary_core::version::Build) -> i32 {
    if !args.is_empty() {
        let _ = writeln!(std::io::stderr(), "usage: video-check");
        return 2;
    }
    let Ok(model) = aviary_core::env::try_required(MODEL_ENV) else {
        let _ = writeln!(
            std::io::stderr(),
            "video-check: no {MODEL_ENV}: run it from the conversation's shell"
        );
        return 1;
    };
    // the sandbox's proxy, as the shell's other commands reach the fixed names
    let proxy = aviary_core::env::try_required("HTTP_PROXY").or_else(|_| aviary_core::env::try_required("http_proxy"));
    let mut b = aviary_core::http::client_builder(build, None).no_proxy();
    if let Ok(p) = proxy {
        match reqwest::Proxy::all(&p) {
            Ok(p) => b = b.proxy(p),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "video-check: the proxy URL: {e}");
                return 1;
            }
        }
    }
    let (Ok(http), Ok(rt)) = (
        b.build(),
        tokio::runtime::Builder::new_current_thread().enable_all().build(),
    ) else {
        return 1;
    };
    let (text, code) = rt.block_on(check(&http, crate::model::BASE, &model));
    if code == 1 {
        let _ = writeln!(std::io::stderr(), "{text}");
    } else {
        let _ = writeln!(std::io::stdout(), "{text}");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_a_yes() {
        assert_eq!(
            verdict(200, br#"{"choices":[{"message":{"content":"Red"}}]}"#),
            Verdict::Sees
        );
    }

    #[test]
    fn no_endpoint_for_the_media_is_a_no() {
        // a model route's answer for a model without sound or video, as seen on 2026-11-18
        let body = br#"{"error":{"message":"No endpoints found that support input audio","code":404}}"#;
        assert_eq!(
            verdict(404, body),
            Verdict::Blind("No endpoints found that support input audio".into())
        );
        assert!(matches!(
            verdict(
                404,
                br#"{"error":{"message":"No endpoints found that support input video"}}"#
            ),
            Verdict::Blind(_)
        ));
    }

    #[test]
    fn any_other_refusal_cannot_tell() {
        assert!(matches!(verdict(502, b"bad gateway"), Verdict::Unknown(w) if w.contains("502: bad gateway")));
        assert!(matches!(
            verdict(429, br#"{"error":{"message":"Rate limit exceeded"}}"#),
            Verdict::Unknown(w) if w.contains("Rate limit")
        ));
    }

    #[test]
    fn the_probe_carries_a_video_and_a_sound_for_the_model() {
        let b = probe_body("xiaomi/mimo-v2.6-pro");
        assert_eq!(b["model"], "xiaomi/mimo-v2.6-pro");
        let parts = b["messages"][0]["content"].as_array().unwrap();
        assert!(
            parts[1]["video_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:video/mp4;base64,AAAA")
        );
        assert_eq!(parts[2]["input_audio"]["format"], "mp3");
        assert!(!parts[2]["input_audio"]["data"].as_str().unwrap().starts_with("data:"));
    }

    #[test]
    fn the_report_names_the_models_that_do() {
        let (t, c) = report(
            "DeepSeek V4.1 Flash",
            &Verdict::Blind("no".into()),
            &["MiMo v2.6 Pro".into()],
        );
        assert_eq!(c, EXIT_BLIND);
        assert!(
            t.contains("cannot watch video or hear sound") && t.contains("Models that do: MiMo v2.6 Pro."),
            "{t}"
        );
        let (t, c) = report("MiMo v2.6 Pro", &Verdict::Sees, &[]);
        assert_eq!(c, 0);
        assert!(t.contains("watches video and hears sound"), "{t}");
        assert_eq!(report("x", &Verdict::Unknown("down".into()), &[]).1, 1);
    }
}
