//! The deployment's settings: what is Deyao's own about his chat, read from a file that lives in his deployment's
//! repo (DE0CH/deyao-ai `k8s/chat/settings.json`), never compiled into this crate (Deyao, 2026-11-13: deployment and
//! customisation live in the deployment's repo, as YYC's do in `hermit.json`). hermit-jarvis reads it at its start
//! (`HERMIT_SETTINGS`, a path) and hands it to each machine's gateway in the boot reply, so the gateway routes a
//! model the way the deployment says without holding a copy of its own.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The context window of a model that names none in the settings file: 1M (Deyao, 2026-11-16: most models have it
/// now, and those to come start there); a model whose window is smaller names it (`contextTokens`). The supervisor
/// hands it to Claude Code, which takes it for a model id it does not recognise (README "Decisions", the models).
pub const DEFAULT_CONTEXT_TOKENS: u64 = 1_000_000;

/// The settings file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    /// What the chat calls itself, and its language.
    pub product: Product,
    /// Claude Code's small model and the title model (a Claude model: it runs on the session's Claude login).
    pub small_model: String,
    /// The models a conversation's menu offers, the default first. A session starts on the model picked in the
    /// Jarvis 4 app.
    pub models: Vec<Model>,
    /// How people sign in beyond the browser. Read here and answered to pigeon (`/v1/deployment`); a machine's
    /// gateway has no use for it and never gets it: a session on an older image refuses a member it does not
    /// know, and a change here must not reach running sessions.
    #[serde(default, skip_serializing)]
    pub sign_in: SignIn,
    /// The conversation search's embedding model (`{"id", "queryInstruction"?}`; Deyao, 2026-11-17): the models
    /// route names it, the supervisor embeds its conversation's messages with it through the machine's gateway, and
    /// pigeon a query through :8082. Absent: the search matches words only. Never serialized: a machine gets it as
    /// the boot reply's own `embedding`, since a session on an older image refuses a settings member it does not
    /// know.
    #[serde(default, skip_serializing)]
    pub embedding: Option<gateway_api::models::Embedding>,
    /// App features this deployment turns on, answered to pigeon in `/v1/deployment` (hermit's `features`), e.g.
    /// `{"personCards": true}` (Deyao, 2026-11-19: the question cards, one deployment first). Never serialized: a
    /// session on an older image refuses a member it does not know.
    #[serde(default, skip_serializing)]
    pub features: Features,
}

/// `features`: each off unless the file turns it on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Features {
    /// The question and wait cards in the chat (aviary magpie rule 11; pigeon's `/api/jarvis/me` `personCards`).
    #[serde(default)]
    pub person_cards: bool,
}

/// `signIn`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SignIn {
    /// The URL scheme of the deployment's phone app (its bundle id): pigeon's `/api/jarvis/app-signin` hands the
    /// person's Access token back to `<scheme>://signed-in`. None: the deployment has no app, and the route is 404.
    #[serde(default)]
    pub app_scheme: Option<String>,
}

/// `product`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Product {
    pub name: String,
    /// `en`, `zh-HK`, …
    pub locale: String,
}

/// One model of the menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Model {
    /// The id Claude Code sends. One with a `/` (`maker/model`) is OpenRouter's; any other runs on the session's
    /// Claude login.
    pub id: String,
    pub label: String,
    /// The thinking levels a person may choose, lowest first (`none` = thinking off, then Claude Code's effort
    /// levels). A model whose hosts refuse thinking switched off lists no `none`.
    pub thinking: Vec<String>,
    /// The level a conversation on this model runs at until a person picks another (the menu's preselected level).
    /// Every model names its own, one of its `thinking` (Deyao, 2026-11-17: a per-model choice, no default): a file
    /// that leaves it out is refused at the start, and DE0CH/deyao-ai's CI refuses it before that
    /// (`hermit-jarvis check-settings`). Never serialized: a machine gets the levels as the boot reply's own
    /// `reasoning` ([`Settings::reasoning`]), since a session on an older image refuses a model member it does not
    /// know.
    #[serde(default, skip_serializing)]
    pub reasoning: Option<String>,
    /// The model's context window when it is not [`DEFAULT_CONTEXT_TOKENS`]: the supervisor runs Claude Code at it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    /// How an OpenRouter model is routed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter: Option<OpenRouter>,
}

/// `models[].openrouter`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenRouter {
    /// The hosts the model's calls are tried on first, in order (`provider.order`); OpenRouter's others stay as the
    /// fallback.
    pub providers: Vec<String>,
}

impl Settings {
    /// Reads and checks the file at `path`.
    ///
    /// # Errors
    /// An unreadable file, JSON that is not the settings, or settings that cannot work (no model, an empty name).
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("settings {path}: {e}"))?;
        Self::parse(&text).map_err(|e| anyhow::anyhow!("settings {path}: {e}"))
    }

    /// The settings of `text`.
    ///
    /// # Errors
    /// As [`Settings::load`].
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let s: Self = serde_json::from_str(text)?;
        anyhow::ensure!(!s.product.name.trim().is_empty(), "product.name is empty");
        anyhow::ensure!(!s.small_model.trim().is_empty(), "smallModel is empty");
        anyhow::ensure!(
            !s.models.is_empty(),
            "models is empty: a conversation needs one"
        );
        anyhow::ensure!(
            s.sign_in.app_scheme.as_deref().is_none_or(url_scheme),
            "signIn.appScheme is not a URL scheme (a letter, then letters, digits, `+`, `-`, `.`)"
        );
        for (i, m) in s.models.iter().enumerate() {
            anyhow::ensure!(
                !m.id.trim().is_empty() && !m.label.trim().is_empty(),
                "models[{i}]: id and label"
            );
            anyhow::ensure!(
                s.models.iter().filter(|o| o.id == m.id).count() == 1,
                "models: {} is listed twice",
                m.id
            );
            anyhow::ensure!(
                !m.thinking.is_empty(),
                "models[{i}] {}: no thinking levels",
                m.id
            );
            let Some(level) = m.reasoning.as_deref() else {
                anyhow::bail!(
                    "models[{i}] {}: no `reasoning` (the level its conversations run at until a person picks \
                     another; every model names its own, there is no default)",
                    m.id
                );
            };
            anyhow::ensure!(
                m.thinking.iter().any(|l| l == level),
                "models[{i}] {}: `reasoning` {level:?} is not one of its thinking levels {:?}",
                m.id,
                m.thinking
            );
            anyhow::ensure!(
                m.openrouter.is_none() || openrouter(&m.id),
                "models[{i}] {}: openrouter routing on a model that is not OpenRouter's (no `/` in its id)",
                m.id
            );
        }
        Ok(s)
    }

    /// The model `id`, when the menu has it.
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&Model> {
        self.models.iter().find(|m| m.id == id)
    }

    /// Each model's level (`reasoning`), as the boot reply hands it to a machine.
    #[must_use]
    pub fn reasoning(&self) -> BTreeMap<String, String> {
        self.models
            .iter()
            .filter_map(|m| Some((m.id.clone(), m.reasoning.clone()?)))
            .collect()
    }

    /// The settings with the boot reply's levels put back on their models (a machine's gateway, after its boot).
    #[must_use]
    pub fn with_reasoning(mut self, levels: &BTreeMap<String, String>) -> Self {
        for m in &mut self.models {
            if let Some(l) = levels.get(&m.id) {
                m.reasoning = Some(l.clone());
            }
        }
        self
    }

    /// The models route's answer (the cluster's `model` name for pigeon; the machine's for its supervisor).
    #[must_use]
    pub fn models_route(&self) -> gateway_api::models::Models {
        gateway_api::models::Models {
            default: self
                .models
                .first()
                .map(|m| m.id.clone())
                .unwrap_or_default(),
            small: self.small_model.clone(),
            title: self.small_model.clone(),
            context_tokens: DEFAULT_CONTEXT_TOKENS,
            // each entry through its JSON: a member a newer aviary adds to `Model` (an optional one, as the
            // contract's are) then needs no change here, so the pin job's bump of the aviary crates never breaks
            // this build
            models: self
                .models
                .iter()
                .filter_map(|m| {
                    let mut v = serde_json::json!({
                        "id": m.id, "label": m.label, "reasoning": m.reasoning, "thinking": m.thinking,
                    });
                    if let Some(tokens) = m.context_tokens {
                        v["contextTokens"] = serde_json::json!(tokens);
                    }
                    serde_json::from_value(v).ok()
                })
                .collect(),
            embedding: self.embedding.clone(),
        }
    }
}

/// Between a Claude model's id and the Claude account it runs on, in the chat's menu (`claude-opus-5-5+chendeyao-uk`):
/// one entry per model and account (Deyao, 2026-11-18: "Opus 5.5 · chendeyao000 or Opus 5.5 · chendeyao-uk" in the
/// model menu). Claude Code takes such an id as the model it starts with (2.1.325 sends the same body for it as for
/// `claude-opus-5-5`, but for the id), and the machine's gateway takes the account off before Anthropic sees it
/// (machine.rs). Not `@`: that is a version in Vertex's model ids. An account's name never has a `+`
/// (the router's `claude-[a-z0-9]+(-[a-z0-9]+)*` stores).
pub const ACCOUNT_SEP: char = '+';

/// A menu id's model and the Claude account it names (`None`: a plain id, which runs on whatever account the session
/// is on). OpenRouter's ids never name one.
#[must_use]
pub fn split_account(id: &str) -> (&str, Option<&str>) {
    if openrouter(id) {
        return (id, None);
    }
    match id.rsplit_once(ACCOUNT_SEP) {
        Some((model, account)) if !model.is_empty() && account_name(account) => {
            (model, Some(account))
        }
        _ => (id, None),
    }
}

/// Whether `s` can be an account's name: the router's store name without `claude-`.
#[must_use]
pub fn account_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 41
        && s.split('-').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

impl Settings {
    /// The menu with each Claude model once per account, in the order given (the first account's entries come first
    /// of each model's, so the first model on the first account is the default): `claude-opus-5-5+chendeyao000`,
    /// labelled "Opus 5.5 · chendeyao000". OpenRouter's models are as they were. No accounts: the menu as the file has
    /// it (the router not read yet), whose plain ids run on the session's own account.
    #[must_use]
    pub fn for_accounts(&self, accounts: &[String]) -> Self {
        if accounts.is_empty() {
            return self.clone();
        }
        let mut out = self.clone();
        out.models = self
            .models
            .iter()
            .flat_map(|m| {
                if openrouter(&m.id) {
                    return vec![m.clone()];
                }
                accounts
                    .iter()
                    .map(|a| Model {
                        id: format!("{}{ACCOUNT_SEP}{a}", m.id),
                        label: format!("{} · {a}", m.label),
                        ..m.clone()
                    })
                    .collect()
            })
            .collect();
        out
    }
}

/// Whether `s` can be a URL's scheme (RFC 3986).
fn url_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// Whether a model id is OpenRouter's (`maker/model`); a Claude id has no `/`.
#[must_use]
pub fn openrouter(model: &str) -> bool {
    model.contains('/')
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const FILE: &str = include_str!("../tests/settings.json");

    #[test]
    fn the_settings_file_gives_the_product_the_menu_and_the_routing() {
        let s = Settings::parse(FILE).unwrap();
        assert_eq!(
            (s.product.name.as_str(), s.product.locale.as_str()),
            ("Jarvis 4", "en")
        );
        let route = s.models_route();
        assert_eq!(
            route.default, "claude-opus-5-5",
            "the first model is the default"
        );
        assert_eq!(route.small, "claude-haiku-5-5");
        assert_eq!(route.models.len(), 3);
        let glm = route
            .models
            .iter()
            .find(|m| m.id == "z-ai/glm-5.3")
            .unwrap();
        assert_eq!(glm.context_tokens, Some(1_000_000));
        assert!(
            !glm.thinking.iter().any(|l| l == "none"),
            "its hosts refuse thinking off"
        );
        assert_eq!(
            route.models[0].context_tokens, None,
            "a model that names no window has the default"
        );
        assert_eq!(
            s.model("z-ai/glm-5.3")
                .unwrap()
                .openrouter
                .as_ref()
                .unwrap()
                .providers,
            ["z-ai/fp8"]
        );
        // every model's own level is the file's: a conversation runs at it until a person picks another
        let levels: Vec<_> = route
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.reasoning.as_deref()))
            .collect();
        assert_eq!(
            levels,
            [
                ("claude-opus-5-5", Some("high")),
                ("claude-fable-5-2", Some("medium")),
                ("z-ai/glm-5.3", Some("low"))
            ]
        );
        assert_eq!(route.context_of("z-ai/glm-5.3"), 1_000_000);
        // the default is 1M (Deyao, 2026-11-16): a model with a smaller window names it
        assert_eq!(route.context_of("claude-fable-5-2"), 1_000_000);
        let mut small = s.clone();
        small.models[0].context_tokens = Some(200_000);
        assert_eq!(small.models_route().context_of("claude-opus-5-5"), 200_000);
        // what a machine's gateway gets in the boot reply is the same settings, less the sign-in (pigeon's), with
        // the levels beside them (a session on an older image refuses a model member it does not know)
        let sent = serde_json::to_value(&s).unwrap();
        assert!(
            sent["models"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m.get("reasoning").is_none()),
            "{sent}"
        );
        // the search's embedding model: the models route names it; a machine gets it beside the settings too
        assert_eq!(
            route.embedding.as_ref().map(|e| e.id.as_str()),
            Some("qwen/qwen3-embedding-8b")
        );
        assert!(sent.get("embedding").is_none(), "{sent}");
        let again: Settings = serde_json::from_value(sent).unwrap();
        assert_eq!(
            again.with_reasoning(&s.reasoning()),
            Settings {
                sign_in: SignIn::default(),
                embedding: None,
                features: Features::default(),
                ..s.clone()
            }
        );
    }

    #[test]
    fn settings_that_cannot_work_are_refused_at_the_start() {
        let bad = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v: serde_json::Value = serde_json::from_str(FILE).unwrap();
            edit(&mut v);
            Settings::parse(&v.to_string()).unwrap_err().to_string()
        };
        assert!(bad(&|v| v["models"] = serde_json::json!([])).contains("models is empty"));
        assert!(bad(&|v| v["product"]["name"] = serde_json::json!(" ")).contains("product.name"));
        assert!(
            bad(&|v| v["models"][1]["id"] = serde_json::json!("claude-opus-5-5"))
                .contains("listed twice")
        );
        assert!(
            bad(&|v| v["models"][0]["openrouter"] = serde_json::json!({"providers": ["x"]}))
                .contains("not OpenRouter's")
        );
        // every model names its own level, one of its thinking levels: there is no default (Deyao, 2026-11-17)
        let missing = bad(&|v| {
            v["models"][1].as_object_mut().unwrap().remove("reasoning");
        });
        assert!(
            missing.contains("claude-fable-5-2") && missing.contains("no `reasoning`"),
            "{missing}"
        );
        assert!(
            bad(&|v| v["models"][2]["reasoning"] = serde_json::json!("none"))
                .contains("not one of its thinking levels")
        );
        assert!(
            bad(&|v| v["models"][0]["thinking"] = serde_json::json!([]))
                .contains("no thinking levels")
        );
        // a key this build does not know is a mistake in the file, not something to skip
        assert!(bad(&|v| v["modles"] = serde_json::json!([])).contains("unknown field"));
    }

    #[test]
    fn the_apps_sign_in_scheme_is_the_files_and_never_a_machines() {
        let s = Settings::parse(FILE).unwrap();
        assert_eq!(
            s.sign_in.app_scheme.as_deref(),
            Some("dev.de0ch.jarvis4chat")
        );
        // what the boot reply hands a machine's gateway: a session on an older image refuses an unknown member
        let sent = serde_json::to_value(&s).unwrap();
        assert!(sent.get("signIn").is_none(), "{sent}");

        // a deployment with no app names none
        let mut file: serde_json::Value = serde_json::from_str(FILE).unwrap();
        file.as_object_mut().unwrap().remove("signIn");
        let none = Settings::parse(&file.to_string()).unwrap();
        assert_eq!(none.sign_in.app_scheme, None);

        // something that cannot be a URL's scheme is refused at the start, not at somebody's sign-in
        file["signIn"] = serde_json::json!({ "appScheme": "not a scheme://" });
        assert!(Settings::parse(&file.to_string()).is_err());
    }

    #[test]
    fn each_claude_model_is_offered_once_per_account() {
        let s = Settings::parse(FILE).unwrap();
        let accounts = vec!["chendeyao000".to_owned(), "chendeyao-uk".to_owned()];
        let menu = s.for_accounts(&accounts).models_route();
        let ids: Vec<(&str, &str)> = menu
            .models
            .iter()
            .map(|m| (m.id.as_str(), m.label.as_str()))
            .collect();
        assert_eq!(
            ids,
            [
                ("claude-opus-5-5+chendeyao000", "Opus 5.5 · chendeyao000"),
                ("claude-opus-5-5+chendeyao-uk", "Opus 5.5 · chendeyao-uk"),
                ("claude-fable-5-2+chendeyao000", "Fable 5.2 · chendeyao000"),
                ("claude-fable-5-2+chendeyao-uk", "Fable 5.2 · chendeyao-uk"),
                ("z-ai/glm-5.3", "GLM 5.3"),
            ]
        );
        assert_eq!(
            menu.default, "claude-opus-5-5+chendeyao000",
            "the first model on the first account"
        );
        assert_eq!(
            menu.small, "claude-haiku-5-5",
            "the small model runs on the account the session is on"
        );
        // each entry keeps its model's own level and thinking row
        let expanded = s.for_accounts(&accounts);
        assert_eq!(
            expanded
                .reasoning()
                .get("claude-fable-5-2+chendeyao-uk")
                .map(String::as_str),
            Some("medium")
        );
        assert_eq!(menu.models[1].thinking, menu.models[0].thinking);
        // the router not read yet: the file's menu, whose plain ids run on the session's own account
        assert_eq!(s.for_accounts(&[]), s);
        // and what a machine's gateway gets back from the boot reply parses as settings
        let sent = serde_json::to_string(&expanded).unwrap();
        let back: Settings = serde_json::from_str(&sent).unwrap();
        assert_eq!(back.models.len(), 5);
    }

    #[test]
    fn a_menu_id_names_its_model_and_account() {
        assert_eq!(
            split_account("claude-opus-5-5+chendeyao-uk"),
            ("claude-opus-5-5", Some("chendeyao-uk"))
        );
        assert_eq!(split_account("claude-opus-5-5"), ("claude-opus-5-5", None));
        // OpenRouter's ids never name an account, whatever they hold
        assert_eq!(split_account("z-ai/glm-5.3+x"), ("z-ai/glm-5.3+x", None));
        // nothing that cannot be an account's name
        for id in [
            "claude-opus-5-5+",
            "+uk",
            "claude-opus-5-5+UK",
            "claude-opus-5-5+a--b",
            "claude-opus-5-5+a b",
        ] {
            assert_eq!(split_account(id), (id, None), "{id}");
        }
    }
}
