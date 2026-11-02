//! The sentences the socket shows (spec 7.9 and the fixed strings of 7.4-7.5, 6.4): one table compiled in, Traditional
//! Chinese (Hong Kong usage) when the deployment's locale starts with `zh`, else English. pigeon never matches message
//! text: model errors and turn failures arrive with magpie's dotted data code, and the sentence is chosen by code.

use magpie_api::errors::{DataCode, ErrorInfo};

/// Which strings a socket uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    /// Traditional Chinese, Hong Kong usage.
    Zh,
    /// English.
    En,
}

impl Locale {
    /// `zh*` → [`Locale::Zh`] (spec 4.1).
    #[must_use]
    pub fn from_zh(zh: bool) -> Self {
        if zh { Self::Zh } else { Self::En }
    }

    fn pick(self, zh: &'static str, en: &'static str) -> &'static str {
        match self {
            Self::Zh => zh,
            Self::En => en,
        }
    }

    /// A conversation with no title (7.4).
    #[must_use]
    pub fn untitled(self) -> &'static str {
        self.pick("新對話", "New conversation")
    }

    /// A send while a reply runs (7.5.4, magpie 409 `turn_running`).
    #[must_use]
    pub fn turn_running(self) -> &'static str {
        self.pick(
            "對話仍在進行中，請稍候。",
            "This conversation is still replying. Please wait.",
        )
    }

    /// A send to a deleted conversation (7.5.4).
    #[must_use]
    pub fn conversation_deleted(self) -> &'static str {
        self.pick("這個對話已刪除。", "This conversation was deleted.")
    }

    /// magpie unreachable, timed out or 5xx on a request (7.5.4).
    #[must_use]
    pub fn unreachable(self) -> &'static str {
        self.pick(
            "暫時連接不到這個對話，請再試一次。",
            "Could not reach this conversation. Please try again.",
        )
    }

    /// A create that failed (7.5.5).
    #[must_use]
    pub fn create_failed(self) -> &'static str {
        self.pick(
            "未能開始對話，請再試一次。",
            "Could not start the conversation. Please try again.",
        )
    }

    /// An empty workspace title (7.5.7).
    #[must_use]
    pub fn empty_title(self) -> &'static str {
        self.pick("名稱不能留空。", "The name can't be empty.")
    }

    /// magpie could not write the view-as audit row (6.4).
    #[must_use]
    pub fn view_as_not_recorded(self) -> &'static str {
        self.pick(
            "未能傳送：未能寫入代看記錄，請再試一次。",
            "Not sent: the view-as log could not be written. Try again.",
        )
    }

    /// An upload magpie refused as too large (5.7, 413).
    #[must_use]
    pub fn too_large(self) -> &'static str {
        self.pick("檔案太大（上限 50 MB）。", "The file is too large (limit 50 MB).")
    }

    /// An upload magpie refused by type (5.7, 415).
    #[must_use]
    pub fn unsupported_type(self) -> &'static str {
        self.pick("不支援這種圖片格式。", "This image format is not supported.")
    }

    /// An image of a send that could not be uploaded (7.5.4).
    #[must_use]
    pub fn upload_failed(self) -> &'static str {
        self.pick(
            "未能上載圖片，訊息未有傳送，請再試一次。",
            "An image could not be uploaded, so the message was not sent. Please try again.",
        )
    }

    /// The line a truncated tool output ends with (7.8).
    #[must_use]
    pub fn output_truncated(self) -> &'static str {
        self.pick(
            "…（輸出過長，只顯示開頭部分）",
            "… (output too long; only the start is shown)",
        )
    }

    /// A failed turn with no code (7.6.2).
    #[must_use]
    pub fn turn_failed(self) -> &'static str {
        self.pick("這次回覆失敗。", "The turn failed.")
    }

    /// The sentence for a data code (7.9), or `None` for a code with none ("other": the message as given).
    #[must_use]
    pub fn code_sentence(self, code: &DataCode) -> Option<&'static str> {
        use DataCode as C;
        Some(match code {
            C::SandboxUnreachable => self.pick(
                "暫時連接不到這個對話的電腦，請稍後再試。",
                "Could not reach the computer for this conversation. Please try again in a minute.",
            ),
            C::SandboxBusy => self.pick(
                "所有電腦正在使用中，請過幾分鐘再試。",
                "All computers are busy right now. Please try again in a few minutes.",
            ),
            C::SandboxNotAllowed => self.pick(
                "這個帳戶不可使用此服務，請聯絡管理員。",
                "This account is not allowed to use this service. Please ask your administrator.",
            ),
            C::SandboxFailed => self.pick(
                "未能啟動這個對話的電腦，請再試一次；如問題持續，請通知管理員。",
                "Could not start the computer for this conversation. Please try again; if it keeps happening, tell \
                 your administrator.",
            ),
            C::SandboxTimeout => self.pick(
                "準備這個對話的電腦用了太長時間，請再試一次。",
                "Preparing the computer for this conversation took too long. Please try again.",
            ),
            C::SandboxLost => self.pick(
                "這個對話的電腦在回覆途中停止了，回覆未能完成。請再試一次。",
                "The computer for this conversation stopped during the reply, so it is incomplete. Please try again.",
            ),
            C::RunExited | C::RunError | C::Internal => {
                self.pick("這次回覆失敗，請再試一次。", "The reply failed. Please try again.")
            }
            C::RunOom => self.pick(
                "這次工作用盡了電腦的記憶體，回覆未能完成。請把工作拆細一點再試。",
                "This task ran out of the computer's memory, so the reply is incomplete. Please try a smaller step.",
            ),
            C::StopForced => self.pick("已停止回覆。", "The reply was stopped."),
            C::ModelBudget => self.budget(),
            C::ModelRateLimited => self.pick(
                "目前向 AI 模型發出的請求太多，請稍等一會再試。",
                "Too many requests to the AI model right now. Please wait a moment and try again.",
            ),
            C::ModelUnavailable => self.pick(
                "AI 模型目前繁忙或暫時無法使用，請稍後再試。",
                "The AI model is busy or unavailable right now. Please try again in a moment.",
            ),
            C::ModelAccess => self.pick(
                "AI 模型服務拒絕存取，請通知管理員。",
                "The AI model service refused access. Please tell your administrator.",
            ),
            C::ModelConnection => self.pick(
                "連接不到 AI 模型，請再試一次。",
                "Could not reach the AI model. Please try again.",
            ),
            // hermit's budget refusal; magpie-api has no variant for it yet
            C::Other(s) if s == "sandbox.budget" => self.budget(),
            _ => return None,
        })
    }

    fn budget(self) -> &'static str {
        self.pick(
            "此服務的使用預算已用完，請聯絡管理員。",
            "The usage budget for this service has been used up. Please ask your administrator.",
        )
    }

    /// The sentence shown for an error carried as data (7.9): `model.budget` prefers magpie's (egress's) own
    /// message, `model.refused` wraps the provider's words, a code without a sentence shows `message` as given.
    #[must_use]
    pub fn error_sentence(self, e: &ErrorInfo) -> String {
        match &e.code {
            DataCode::ModelBudget if !e.message.trim().is_empty() => e.message.clone(),
            DataCode::ModelRefused => match self {
                Self::Zh => format!("AI 模型拒絕了這個請求：{}", e.message),
                Self::En => format!("The AI model refused this request: {}", e.message),
            },
            code => self
                .code_sentence(code)
                .map_or_else(|| e.message.clone(), str::to_owned),
        }
    }
}

/// `rpc_error` text of an unknown request (7.3).
pub const NOT_AVAILABLE: &str = "Not available here";
/// A conversation that does not exist or is not the person's (7.5.2-7.5.3).
pub const NOT_FOUND: &str = "Conversation not found";
/// The view-as refusal on the socket (6.4).
pub const VIEW_AS_READ_ONLY: &str = "Read-only while viewing as someone else.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_code_has_a_sentence_in_both_locales_except_refused() {
        for code in [
            DataCode::SandboxUnreachable,
            DataCode::SandboxBusy,
            DataCode::SandboxNotAllowed,
            DataCode::SandboxFailed,
            DataCode::SandboxTimeout,
            DataCode::SandboxLost,
            DataCode::RunExited,
            DataCode::RunError,
            DataCode::RunOom,
            DataCode::StopForced,
            DataCode::Internal,
            DataCode::ModelBudget,
            DataCode::ModelRateLimited,
            DataCode::ModelUnavailable,
            DataCode::ModelAccess,
            DataCode::ModelConnection,
        ] {
            let zh = Locale::Zh.code_sentence(&code).unwrap();
            let en = Locale::En.code_sentence(&code).unwrap();
            assert_ne!(zh, en, "{code:?}");
        }
        assert!(Locale::Zh.code_sentence(&DataCode::ModelRefused).is_none());
    }

    #[test]
    fn budget_and_refused_use_the_message() {
        let mut e = ErrorInfo::new(DataCode::ModelBudget, "本月預算已用完。");
        assert_eq!(Locale::En.error_sentence(&e), "本月預算已用完。");
        e.message.clear();
        assert!(Locale::En.error_sentence(&e).starts_with("The usage budget"));
        let r = ErrorInfo::new(DataCode::ModelRefused, "prompt too long");
        assert_eq!(Locale::Zh.error_sentence(&r), "AI 模型拒絕了這個請求：prompt too long");
        let other = ErrorInfo::new(DataCode::from_wire("cc.write_stuck"), "stuck");
        assert_eq!(Locale::Zh.error_sentence(&other), "stuck");
    }
}
