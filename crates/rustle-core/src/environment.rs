//! Runtime environment helpers for desktop-session compatibility.

/// Returns true when the current process appears to be running inside a Hyprland session.
#[must_use]
pub fn is_hyprland_session() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")
        .is_some_and(|value| !value.is_empty())
}

/// Human-readable support statement for the current Rustle runtime contract.
#[must_use]
pub fn supported_session_label() -> &'static str {
    "Hyprland"
}

#[cfg(test)]
mod tests {
    use super::{is_hyprland_session, supported_session_label};

    #[test]
    fn supported_session_label_is_hyprland() {
        assert_eq!(supported_session_label(), "Hyprland");
    }

    #[test]
    fn hyprland_detection_requires_instance_signature() {
        let original = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE");

        unsafe {
            std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
        }
        assert!(!is_hyprland_session());

        unsafe {
            std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", "test-instance");
        }
        assert!(is_hyprland_session());

        match original {
            Some(value) => unsafe {
                std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", value);
            },
            None => unsafe {
                std::env::remove_var("HYPRLAND_INSTANCE_SIGNATURE");
            },
        }
    }
}