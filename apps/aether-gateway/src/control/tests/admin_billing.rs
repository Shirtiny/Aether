use http::Uri;

use super::{classify_control_route, headers, GatewayPublicRequestContext};
use crate::handlers::shared::local_proxy_route_requires_buffered_body;

#[test]
fn classifies_admin_billing_presets_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/billing/presets"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::GET, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(decision.route_kind.as_deref(), Some("list_presets"));
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:billing")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_billing_apply_preset_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/billing/presets/apply"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::POST, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(decision.route_kind.as_deref(), Some("apply_preset"));
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:billing")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_billing_rule_routes_as_admin_proxy_route() {
    let headers = headers(&[]);

    let list_uri: Uri = "/api/admin/billing/rules?page=1"
        .parse()
        .expect("uri should parse");
    let list = classify_control_route(&http::Method::GET, &list_uri, &headers)
        .expect("route should classify");
    assert_eq!(list.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(list.route_kind.as_deref(), Some("list_rules"));

    let detail_uri: Uri = "/api/admin/billing/rules/rule-1"
        .parse()
        .expect("uri should parse");
    let detail = classify_control_route(&http::Method::GET, &detail_uri, &headers)
        .expect("route should classify");
    assert_eq!(detail.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(detail.route_kind.as_deref(), Some("get_rule"));

    let create_uri: Uri = "/api/admin/billing/rules"
        .parse()
        .expect("uri should parse");
    let create = classify_control_route(&http::Method::POST, &create_uri, &headers)
        .expect("route should classify");
    assert_eq!(create.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(create.route_kind.as_deref(), Some("create_rule"));

    let update_uri: Uri = "/api/admin/billing/rules/rule-1"
        .parse()
        .expect("uri should parse");
    let update = classify_control_route(&http::Method::PUT, &update_uri, &headers)
        .expect("route should classify");
    assert_eq!(update.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(update.route_kind.as_deref(), Some("update_rule"));
    assert_eq!(
        update.auth_endpoint_signature.as_deref(),
        Some("admin:billing")
    );
}

#[test]
fn classifies_admin_billing_collector_routes_as_admin_proxy_route() {
    let headers = headers(&[]);

    let list_uri: Uri = "/api/admin/billing/collectors?page=1"
        .parse()
        .expect("uri should parse");
    let list = classify_control_route(&http::Method::GET, &list_uri, &headers)
        .expect("route should classify");
    assert_eq!(list.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(list.route_kind.as_deref(), Some("list_collectors"));

    let detail_uri: Uri = "/api/admin/billing/collectors/collector-1"
        .parse()
        .expect("uri should parse");
    let detail = classify_control_route(&http::Method::GET, &detail_uri, &headers)
        .expect("route should classify");
    assert_eq!(detail.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(detail.route_kind.as_deref(), Some("get_collector"));

    let create_uri: Uri = "/api/admin/billing/collectors"
        .parse()
        .expect("uri should parse");
    let create = classify_control_route(&http::Method::POST, &create_uri, &headers)
        .expect("route should classify");
    assert_eq!(create.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(create.route_kind.as_deref(), Some("create_collector"));

    let update_uri: Uri = "/api/admin/billing/collectors/collector-1"
        .parse()
        .expect("uri should parse");
    let update = classify_control_route(&http::Method::PUT, &update_uri, &headers)
        .expect("route should classify");
    assert_eq!(update.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(update.route_kind.as_deref(), Some("update_collector"));
    assert_eq!(
        update.auth_endpoint_signature.as_deref(),
        Some("admin:billing")
    );
}

#[test]
fn classifies_admin_billing_plan_routes_as_admin_proxy_route() {
    let headers = headers(&[]);

    let list_uri: Uri = "/api/admin/billing/plans"
        .parse()
        .expect("uri should parse");
    let list = classify_control_route(&http::Method::GET, &list_uri, &headers)
        .expect("route should classify");
    assert_eq!(list.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(list.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(list.route_kind.as_deref(), Some("list_plans"));
    assert_eq!(
        list.auth_endpoint_signature.as_deref(),
        Some("admin:billing")
    );

    let create_uri: Uri = "/api/admin/billing/plans"
        .parse()
        .expect("uri should parse");
    let create = classify_control_route(&http::Method::POST, &create_uri, &headers)
        .expect("route should classify");
    assert_eq!(create.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(create.route_kind.as_deref(), Some("create_plan"));

    let update_uri: Uri = "/api/admin/billing/plans/plan-1"
        .parse()
        .expect("uri should parse");
    let update = classify_control_route(&http::Method::PUT, &update_uri, &headers)
        .expect("route should classify");
    assert_eq!(update.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(update.route_kind.as_deref(), Some("update_plan"));

    let status_uri: Uri = "/api/admin/billing/plans/plan-1/status"
        .parse()
        .expect("uri should parse");
    let status = classify_control_route(&http::Method::PATCH, &status_uri, &headers)
        .expect("route should classify");
    assert_eq!(status.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(status.route_kind.as_deref(), Some("set_plan_status"));

    let delete_uri: Uri = "/api/admin/billing/plans/plan-1"
        .parse()
        .expect("uri should parse");
    let delete = classify_control_route(&http::Method::DELETE, &delete_uri, &headers)
        .expect("route should classify");
    assert_eq!(delete.route_family.as_deref(), Some("billing_manage"));
    assert_eq!(delete.route_kind.as_deref(), Some("delete_plan"));
}

#[test]
fn admin_billing_plan_write_routes_buffer_request_body() {
    let headers = headers(&[]);
    let routes = [
        (http::Method::POST, "/api/admin/billing/plans"),
        (http::Method::PUT, "/api/admin/billing/plans/plan-1"),
        (
            http::Method::PATCH,
            "/api/admin/billing/plans/plan-1/status",
        ),
    ];

    for (method, path) in routes {
        let uri: Uri = path.parse().expect("uri should parse");
        let decision =
            classify_control_route(&method, &uri, &headers).expect("route should classify");
        let context = GatewayPublicRequestContext::from_request_parts(
            "trace-billing-plan-write",
            &method,
            &uri,
            &headers,
            Some(decision),
        );

        assert!(
            local_proxy_route_requires_buffered_body(&context),
            "{method} {path} should buffer request body"
        );
    }
}
