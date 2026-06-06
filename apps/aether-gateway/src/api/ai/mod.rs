mod aliyun;
mod claude;
mod doubao;
mod gemini;
mod jina;
mod openai;
mod registry;

pub(crate) use registry::{
    admin_default_body_rules_for_signature, admin_endpoint_signature_parts, mount_ai_routes,
    normalize_admin_endpoint_signature, public_api_format_local_path,
};
