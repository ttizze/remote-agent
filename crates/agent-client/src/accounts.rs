//! Pure account interaction decisions. Native code owns scheduling and projects
//! the typed operation results; it does not choose acceptance or completion rules.

#[repr(u32)]
pub enum Action {
    Ignore = 0,
    BeginSelection = 1,
    BeginLogin = 2,
    ApplyList = 3,
    ApplySelection = 4,
    ApplyLogin = 5,
    CompleteLogin = 6,
    ClearLogin = 7,
    ShowError = 8,
    SelectionFailed = 9,
    LoginFailed = 10,
    ContinuePolling = 11,
}

/// Compact native boundary; event and flag codes are documented in mobile_client.h.
pub fn transition(event: u32, flags: u32) -> Action {
    let failed = flags & 16 != 0;
    match event {
        0 if flags & 3 == 0 => Action::BeginSelection,
        1 if flags & 12 == 0 => Action::BeginLogin,
        2 => {
            if failed {
                Action::ShowError
            } else {
                Action::ApplyList
            }
        }
        3 => {
            if failed {
                Action::SelectionFailed
            } else {
                Action::ApplySelection
            }
        }
        4 => {
            if failed {
                Action::LoginFailed
            } else {
                Action::ApplyLogin
            }
        }
        5 => {
            if failed {
                Action::ShowError
            } else if flags & 32 != 0 {
                Action::CompleteLogin
            } else {
                Action::ContinuePolling
            }
        }
        6 => {
            if failed {
                Action::ShowError
            } else {
                Action::ClearLogin
            }
        }
        _ => Action::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_selection_and_login_do_not_start_another_operation() {
        for flags in [1, 2, 3] {
            assert!(matches!(transition(0, flags), Action::Ignore));
        }
        for flags in [4, 8, 12] {
            assert!(matches!(transition(1, flags), Action::Ignore));
        }
        assert!(matches!(transition(0, 0), Action::BeginSelection));
        assert!(matches!(transition(1, 0), Action::BeginLogin));
    }

    #[test]
    fn failed_poll_never_completes_login_and_failed_cancel_keeps_it_available() {
        assert!(matches!(transition(5, 16 | 32), Action::ShowError));
        assert!(matches!(transition(5, 32), Action::CompleteLogin));
        assert!(matches!(transition(5, 0), Action::ContinuePolling));
        assert!(matches!(transition(6, 16), Action::ShowError));
        assert!(matches!(transition(6, 0), Action::ClearLogin));
    }
}
