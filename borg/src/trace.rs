use crate::types::IngestMethod;

pub fn generate(method: IngestMethod) -> String {
    // IngestMethod IS vault::schema::Method, so pass it straight through.
    vault::trace::generate(method)
}
