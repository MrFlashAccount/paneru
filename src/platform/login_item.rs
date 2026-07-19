use std::path::Path;

use objc2_service_management::{SMAppService, SMAppServiceStatus};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoginItemStatus {
    Unavailable,
    NotRegistered,
    Enabled,
    RequiresApproval,
    NotFound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoginItemAction {
    Changed,
    OpenedSystemSettings,
}

pub(crate) fn status() -> LoginItemStatus {
    if !is_available() {
        return LoginItemStatus::Unavailable;
    }

    let service = unsafe { SMAppService::mainAppService() };
    map_status(unsafe { service.status() })
}

pub(crate) fn toggle() -> Result<LoginItemAction, String> {
    match status() {
        LoginItemStatus::Unavailable => Err(
            "Launch at Login requires macOS 13 or later and an installed Paneru.app bundle."
                .to_owned(),
        ),
        LoginItemStatus::Enabled => {
            let service = unsafe { SMAppService::mainAppService() };
            unsafe { service.unregisterAndReturnError() }
                .map_err(|error| error.localizedDescription().to_string())?;
            Ok(LoginItemAction::Changed)
        }
        LoginItemStatus::RequiresApproval => {
            unsafe { SMAppService::openSystemSettingsLoginItems() };
            Ok(LoginItemAction::OpenedSystemSettings)
        }
        LoginItemStatus::NotRegistered | LoginItemStatus::NotFound => {
            let service = unsafe { SMAppService::mainAppService() };
            unsafe { service.registerAndReturnError() }
                .map_err(|error| error.localizedDescription().to_string())?;
            if status() == LoginItemStatus::RequiresApproval {
                unsafe { SMAppService::openSystemSettingsLoginItems() };
                Ok(LoginItemAction::OpenedSystemSettings)
            } else {
                Ok(LoginItemAction::Changed)
            }
        }
    }
}

fn is_available() -> bool {
    objc2::available!(macos = 13.0)
        && std::env::current_exe().is_ok_and(|path| is_app_bundle_executable(&path))
}

fn is_app_bundle_executable(path: &Path) -> bool {
    let Some(macos_dir) = path.parent() else {
        return false;
    };
    let Some(contents_dir) = macos_dir.parent() else {
        return false;
    };
    let Some(app_dir) = contents_dir.parent() else {
        return false;
    };

    macos_dir.file_name().is_some_and(|name| name == "MacOS")
        && contents_dir
            .file_name()
            .is_some_and(|name| name == "Contents")
        && app_dir
            .extension()
            .is_some_and(|extension| extension == "app")
}

fn map_status(status: SMAppServiceStatus) -> LoginItemStatus {
    match status {
        SMAppServiceStatus::NotRegistered => LoginItemStatus::NotRegistered,
        SMAppServiceStatus::Enabled => LoginItemStatus::Enabled,
        SMAppServiceStatus::RequiresApproval => LoginItemStatus::RequiresApproval,
        _ => LoginItemStatus::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::{LoginItemStatus, is_app_bundle_executable, map_status};
    use objc2_service_management::SMAppServiceStatus;
    use std::path::Path;

    #[test]
    fn recognizes_main_app_bundle_executable() {
        assert!(is_app_bundle_executable(Path::new(
            "/Applications/Paneru.app/Contents/MacOS/paneru"
        )));
        assert!(!is_app_bundle_executable(Path::new(
            "/tmp/target/release/paneru"
        )));
        assert!(!is_app_bundle_executable(Path::new(
            "/Applications/Paneru/Contents/MacOS/paneru"
        )));
    }

    #[test]
    fn maps_native_service_statuses() {
        assert_eq!(
            map_status(SMAppServiceStatus::NotRegistered),
            LoginItemStatus::NotRegistered
        );
        assert_eq!(
            map_status(SMAppServiceStatus::Enabled),
            LoginItemStatus::Enabled
        );
        assert_eq!(
            map_status(SMAppServiceStatus::RequiresApproval),
            LoginItemStatus::RequiresApproval
        );
        assert_eq!(
            map_status(SMAppServiceStatus::NotFound),
            LoginItemStatus::NotFound
        );
    }
}
