use http::Uri;

use super::{classify_control_route, headers};

fn assert_proxy_nodes_admin_route(method: http::Method, path: &str, route_kind: &str) {
    let headers = headers(&[]);
    let uri: Uri = path.parse().expect("uri should parse");
    let decision = classify_control_route(&method, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(decision.route_kind.as_deref(), Some(route_kind));
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_list_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes?status=online&skip=10&limit=20"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::GET, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(decision.route_kind.as_deref(), Some("list_nodes"));
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_register_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes/register"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::POST, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(decision.route_kind.as_deref(), Some("register_node"));
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_heartbeat_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::POST,
        "/api/admin/proxy-nodes/heartbeat",
        "heartbeat_node",
    );
}

#[test]
fn classifies_admin_proxy_nodes_unregister_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::POST,
        "/api/admin/proxy-nodes/unregister",
        "unregister_node",
    );
}

#[test]
fn classifies_admin_proxy_nodes_manual_create_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::POST,
        "/api/admin/proxy-nodes/manual",
        "create_manual_node",
    );
}

#[test]
fn classifies_admin_proxy_nodes_install_session_create_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::POST,
        "/api/admin/proxy-nodes/install-sessions",
        "create_proxy_node_install_session",
    );
}

#[test]
fn classifies_admin_proxy_nodes_manual_update_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::PATCH,
        "/api/admin/proxy-nodes/node-1",
        "update_manual_node",
    );
}

#[test]
fn classifies_admin_proxy_nodes_detail_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::GET,
        "/api/admin/proxy-nodes/node-1",
        "get_node",
    );
}

#[test]
fn classifies_admin_proxy_node_metrics_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::GET,
        "/api/admin/proxy-nodes/node-1/metrics?from=1700000000&to=1700003600&step=1m",
        "list_node_metrics",
    );
}

#[test]
fn classifies_admin_proxy_fleet_metrics_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::GET,
        "/api/admin/proxy-nodes/metrics/fleet?from=1700000000&to=1700003600&step=1m",
        "list_fleet_metrics",
    );
}

#[test]
fn classifies_admin_proxy_nodes_delete_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::DELETE,
        "/api/admin/proxy-nodes/node-1",
        "delete_node",
    );
}

#[test]
fn classifies_admin_proxy_nodes_test_node_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::POST,
        "/api/admin/proxy-nodes/node-1/test",
        "test_node",
    );
}

#[test]
fn classifies_admin_proxy_nodes_test_url_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::POST,
        "/api/admin/proxy-nodes/test-url",
        "test_proxy_url",
    );
}

#[test]
fn classifies_admin_proxy_nodes_update_config_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::PUT,
        "/api/admin/proxy-nodes/node-1/config",
        "update_node_config",
    );
}

#[test]
fn classifies_admin_proxy_nodes_upgrade_start_as_admin_proxy_route() {
    assert_proxy_nodes_admin_route(
        http::Method::POST,
        "/api/admin/proxy-nodes/upgrade",
        "batch_upgrade_nodes",
    );
}

#[test]
fn classifies_admin_proxy_nodes_events_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes/node-1/events?limit=50"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::GET, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(decision.route_kind.as_deref(), Some("list_node_events"));
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_upgrade_cancel_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes/upgrade/cancel"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::POST, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(
        decision.route_kind.as_deref(),
        Some("cancel_upgrade_rollout")
    );
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_upgrade_clear_conflicts_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes/upgrade/clear-conflicts"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::POST, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(
        decision.route_kind.as_deref(),
        Some("clear_upgrade_rollout_conflicts")
    );
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_upgrade_restore_skipped_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes/upgrade/restore-skipped"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::POST, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(
        decision.route_kind.as_deref(),
        Some("restore_skipped_upgrade_rollout_nodes")
    );
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_upgrade_skip_node_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes/node-1/upgrade/skip"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::POST, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(
        decision.route_kind.as_deref(),
        Some("skip_upgrade_rollout_node")
    );
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}

#[test]
fn classifies_admin_proxy_nodes_upgrade_retry_node_as_admin_proxy_route() {
    let headers = headers(&[]);
    let uri: Uri = "/api/admin/proxy-nodes/node-1/upgrade/retry"
        .parse()
        .expect("uri should parse");
    let decision =
        classify_control_route(&http::Method::POST, &uri, &headers).expect("route should classify");

    assert_eq!(decision.route_class.as_deref(), Some("admin_proxy"));
    assert_eq!(decision.route_family.as_deref(), Some("proxy_nodes_manage"));
    assert_eq!(
        decision.route_kind.as_deref(),
        Some("retry_upgrade_rollout_node")
    );
    assert_eq!(
        decision.auth_endpoint_signature.as_deref(),
        Some("admin:proxy_nodes")
    );
    assert!(!decision.is_execution_runtime_candidate());
}
