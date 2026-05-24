use crate::types::IngestMethod;
use vault::schema::Method;

pub fn generate(method: IngestMethod) -> String {
    let vault_method = match method {
        IngestMethod::Telegram => Method::Telegram,
        IngestMethod::Discord => Method::Discord,
        IngestMethod::Http => Method::Http,
        IngestMethod::Clipboard => Method::Clipboard,
        IngestMethod::Cli => Method::Cli,
        IngestMethod::Ntfy => Method::Ntfy,
        IngestMethod::Signal => Method::Signal,
    };
    vault::trace::generate(vault_method)
}
