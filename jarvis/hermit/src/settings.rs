//! The deployment's settings: what is Deyao's own about his chat, read from a file that lives in his deployment's
//! repo (DE0CH/deyao-ai `k8s/chat/settings.json`), never compiled into this crate (Deyao, 2026-11-13: deployment and
//! customisation live in the deployment's repo, as YYC's do in `hermit.json`). hermit-jarvis reads it at its start
//! (`HERMIT_SETTINGS`, a path) and hands it to each machine's gateway in the boot reply, so the gateway routes a
//! model the way the deployment says without holding a copy of its own.

use serde::{Deserialize, Serialize};

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
    /// The model's context window when Claude Code does not know it (it takes an unknown model as 200k): the
    /// supervisor runs Claude Code at it. Absent: Claude Code's own.
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
            context_tokens: 0,
            // each entry through its JSON: a member a newer aviary adds to `Model` (an optional one, as the
            // contract's are) then needs no change here, so the pin job's bump of the aviary crates never breaks
            // this build
            models: self
                .models
                .iter()
                .filter_map(|m| {
                    let mut v = serde_json::json!({
                        "id": m.id, "label": m.label, "reasoning": null, "thinking": m.thinking,
                    });
                    if let Some(tokens) = m.context_tokens {
                        v["contextTokens"] = serde_json::json!(tokens);
                    }
                    serde_json::from_value(v).ok()
                })
                .collect(),
        }
    }
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
            route.default, "claude-fable-5-2",
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
            "a Claude model keeps Claude Code's window"
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
        // none is named as a model's own level: with nothing picked, Claude Code keeps its default for the model
        assert!(route.models.iter().all(|m| m.reasoning.is_none()));
        assert_eq!(route.context_of("z-ai/glm-5.3"), 1_000_000);
        assert_eq!(route.context_of("claude-fable-5-2"), 0);
        // what a machine's gateway gets in the boot reply is the same settings
        let again: Settings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(again, s);
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
            bad(&|v| v["models"][1]["id"] = serde_json::json!("claude-fable-5-2"))
                .contains("listed twice")
        );
        assert!(
            bad(&|v| v["models"][0]["openrouter"] = serde_json::json!({"providers": ["x"]}))
                .contains("not OpenRouter's")
        );
        // a key this build does not know is a mistake in the file, not something to skip
        assert!(bad(&|v| v["modles"] = serde_json::json!([])).contains("unknown field"));
    }
}
