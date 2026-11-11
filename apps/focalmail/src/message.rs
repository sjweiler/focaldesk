use crate::{attachments, html};
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct Message {
    pub(crate) id: String,
    pub(crate) sender: String,
    pub(crate) email: String,
    pub(crate) subject: String,
    pub(crate) body: String,
    pub(crate) date: String,
    pub(crate) account: String,
    pub(crate) folder: String,
    pub(crate) unread: bool,
    pub(crate) flagged: bool,
    #[serde(default)]
    pub(crate) labels: Vec<String>,
    #[serde(default)]
    pub(crate) timestamp: i64,
    #[serde(default)]
    pub(crate) attachments: Vec<attachments::Attachment>,
    #[serde(default)]
    pub(crate) attachments_loaded: bool,
    #[serde(default)]
    pub(crate) html: Option<html::Document>,
    #[serde(default)]
    pub(crate) html_loaded: bool,
    #[serde(default)]
    pub(crate) to_header: String,
    #[serde(default)]
    pub(crate) cc_header: String,
    #[serde(default)]
    pub(crate) reply_to: String,
    #[serde(default)]
    pub(crate) delivered_to: String,
    #[serde(default)]
    pub(crate) message_id: String,
    #[serde(default)]
    pub(crate) references: String,
    #[serde(default)]
    pub(crate) thread_id: String,
    #[serde(default)]
    pub(crate) recipients_loaded: bool,
}

impl Message {
    pub(crate) fn update_content(&mut self, loaded: &Message) {
        self.body = loaded.body.clone();
        self.html = loaded.html.clone();
        self.html_loaded = loaded.html_loaded;
        self.attachments = loaded.attachments.clone();
        self.attachments_loaded = loaded.attachments_loaded;
        self.to_header = loaded.to_header.clone();
        self.cc_header = loaded.cc_header.clone();
        self.reply_to = loaded.reply_to.clone();
        self.delivered_to = loaded.delivered_to.clone();
        self.message_id = loaded.message_id.clone();
        self.references = loaded.references.clone();
        self.thread_id = loaded.thread_id.clone();
        self.recipients_loaded = loaded.recipients_loaded;
    }
    pub(crate) fn in_folder(&self, folder: &str) -> bool {
        if self.labels.is_empty() {
            return self.folder == folder;
        }
        let hidden = self
            .labels
            .iter()
            .any(|l| matches!(l.as_str(), "TRASH" | "SPAM" | "DRAFT"));
        match folder {
            "Inbox" => !hidden && self.labels.iter().any(|l| l == "INBOX"),
            "Sent" => !hidden && self.labels.iter().any(|l| l == "SENT"),
            _ => self.folder == folder,
        }
    }
}
