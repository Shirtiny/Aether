use axum::http;

use super::{classified, ClassifiedRoute};

pub(super) fn classify_admin_routing_family_route(
    method: &http::Method,
    normalized_path_no_trailing: &str,
) -> Option<ClassifiedRoute> {
    let path = normalized_path_no_trailing;
    if method == http::Method::GET && path == "/api/admin/routing/groups" {
        Some(routing_route("list_groups"))
    } else if method == http::Method::POST && path == "/api/admin/routing/groups" {
        Some(routing_route("create_group"))
    } else if method == http::Method::GET
        && path.starts_with("/api/admin/routing/groups/")
        && path.ends_with("/versions")
        && path.matches('/').count() == 6
    {
        Some(routing_route("list_group_versions"))
    } else if method == http::Method::POST
        && path.starts_with("/api/admin/routing/groups/")
        && path.ends_with("/publish")
        && path.matches('/').count() == 6
    {
        Some(routing_route("publish_group"))
    } else if method == http::Method::POST
        && path.starts_with("/api/admin/routing/groups/")
        && path.ends_with("/dry-run")
        && path.matches('/').count() == 6
    {
        Some(routing_route("dry_run_group"))
    } else if method == http::Method::GET
        && path.starts_with("/api/admin/routing/groups/")
        && path.matches('/').count() == 5
    {
        Some(routing_route("get_group"))
    } else if method == http::Method::PATCH
        && path.starts_with("/api/admin/routing/groups/")
        && path.matches('/').count() == 5
    {
        Some(routing_route("update_group"))
    } else if method == http::Method::DELETE
        && path.starts_with("/api/admin/routing/groups/")
        && path.matches('/').count() == 5
    {
        Some(routing_route("delete_group"))
    } else if method == http::Method::GET && path == "/api/admin/routing/bindings" {
        Some(routing_route("list_bindings"))
    } else if method == http::Method::POST && path == "/api/admin/routing/bindings" {
        Some(routing_route("create_binding"))
    } else if method == http::Method::PATCH
        && path.starts_with("/api/admin/routing/bindings/")
        && path.matches('/').count() == 5
    {
        Some(routing_route("update_binding"))
    } else if method == http::Method::DELETE
        && path.starts_with("/api/admin/routing/bindings/")
        && path.matches('/').count() == 5
    {
        Some(routing_route("delete_binding"))
    } else {
        None
    }
}

fn routing_route(route_kind: &'static str) -> ClassifiedRoute {
    classified(
        "admin_proxy",
        "routing_profiles_manage",
        route_kind,
        "admin:routing_profiles",
        false,
    )
}
