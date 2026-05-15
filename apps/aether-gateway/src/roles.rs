pub(crate) const ROLE_USER: &str = "user";
pub(crate) const ROLE_ADMIN: &str = "admin";
pub(crate) const ROLE_AUDIT_ADMIN: &str = "audit_admin";

pub(crate) fn is_full_admin_role(role: &str) -> bool {
    role.trim().eq_ignore_ascii_case(ROLE_ADMIN)
}

pub(crate) fn is_audit_admin_role(role: &str) -> bool {
    role.trim().eq_ignore_ascii_case(ROLE_AUDIT_ADMIN)
}

pub(crate) fn can_access_admin_console(role: &str) -> bool {
    is_full_admin_role(role) || is_audit_admin_role(role)
}

pub(crate) fn can_write_admin_console(role: &str) -> bool {
    is_full_admin_role(role)
}

pub(crate) fn normalize_assignable_user_role(role: &str) -> Option<&'static str> {
    match role.trim().to_ascii_lowercase().as_str() {
        ROLE_USER => Some(ROLE_USER),
        ROLE_ADMIN => Some(ROLE_ADMIN),
        ROLE_AUDIT_ADMIN => Some(ROLE_AUDIT_ADMIN),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        can_access_admin_console, can_write_admin_console, normalize_assignable_user_role,
    };

    #[test]
    fn audit_admin_can_access_but_not_write_admin_console() {
        assert!(can_access_admin_console("audit_admin"));
        assert!(!can_write_admin_console("audit_admin"));
    }

    #[test]
    fn assignable_roles_include_audit_admin() {
        assert_eq!(
            normalize_assignable_user_role(" audit_admin "),
            Some("audit_admin")
        );
        assert_eq!(normalize_assignable_user_role("admin"), Some("admin"));
        assert_eq!(normalize_assignable_user_role("user"), Some("user"));
        assert_eq!(normalize_assignable_user_role("owner"), None);
    }
}
