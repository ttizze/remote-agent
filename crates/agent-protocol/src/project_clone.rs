//! Host-owned repository acquisition; clients observe the same revisioned jobs.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneProject {
    pub id: String,
    pub repository: String,
    pub destination: String,
}

impl CloneProject {
    pub fn validate(&self) -> Result<(), String> {
        validate_id(&self.id)?;
        let repository = self.repository.trim();
        if repository != self.repository
            || repository.is_empty()
            || repository.len() > 2048
            || repository.chars().any(char::is_control)
            || repository.starts_with('-')
        {
            return Err("invalid repository address".into());
        }
        if repository.contains("://") {
            let url = url::Url::parse(repository).map_err(|_| "invalid repository URL")?;
            if !matches!(url.scheme(), "https" | "http" | "ssh" | "git" | "file")
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || (matches!(url.scheme(), "https" | "http") && !url.username().is_empty())
            {
                return Err("use a repository URL without embedded credentials".into());
            }
        } else if !crate::paths::is_absolute_host_path(repository)
            && !repository.split_once(':').is_some_and(|(host, path)| {
                !host.is_empty()
                    && !host.contains('/')
                    && !path.is_empty()
                    && !path.starts_with(':')
            })
        {
            return Err("repository must be an absolute path or remote address".into());
        }
        if !crate::paths::is_absolute_host_path(&self.destination)
            || self.destination.chars().any(char::is_control)
        {
            return Err("clone destination must be an absolute Host path".into());
        }
        Ok(())
    }
}

pub fn validate_id(id: &str) -> Result<(), String> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| "clone id must be a UUID".into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadProjectClones {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectCloneAction {
    Cancel,
    Retry,
    Dismiss,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeProjectClone {
    pub id: String,
    pub action: ProjectCloneAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProjectClonePhase {
    Queued,
    Fetching,
    Resolving,
    Checkout,
    Registering,
    Completed,
    Cancelling,
    Cancelled,
    Failed,
}

impl ProjectClonePhase {
    pub fn active(self) -> bool {
        !matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
    pub fn can_cancel(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Fetching | Self::Resolving | Self::Checkout
        )
    }
    pub fn can_retry(self) -> bool {
        matches!(self, Self::Cancelled | Self::Failed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloneProgress {
    pub current: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCloneState {
    pub request: CloneProject,
    pub phase: ProjectClonePhase,
    pub progress: Option<CloneProgress>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectClones {
    /// A fresh epoch prevents a restarted Host's revision from appearing stale.
    pub epoch: String,
    pub revision: u64,
    pub clones: Vec<ProjectCloneState>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controls_follow_the_clone_commit_boundary() {
        use ProjectClonePhase::*;
        for (phase, active, cancel, retry) in [
            (Queued, true, true, false),
            (Fetching, true, true, false),
            (Resolving, true, true, false),
            (Checkout, true, true, false),
            (Registering, true, false, false),
            (Cancelling, true, false, false),
            (Completed, false, false, false),
            (Cancelled, false, false, true),
            (Failed, false, false, true),
        ] {
            assert_eq!(phase.active(), active, "{phase:?}");
            assert_eq!(phase.can_cancel(), cancel, "{phase:?}");
            assert_eq!(phase.can_retry(), retry, "{phase:?}");
        }
    }
    fn request(repository: &str) -> CloneProject {
        CloneProject {
            id: uuid::Uuid::nil().to_string(),
            repository: repository.into(),
            destination: "/projects/new".into(),
        }
    }
    #[test]
    fn addresses_keep_credentials_out_of_stored_jobs() {
        for address in [
            "https://host/repo.git",
            "git@host:owner/repo.git",
            "/local/repo",
            "ssh://git@host/repo",
            "file:///local/repo",
        ] {
            assert!(request(address).validate().is_ok(), "{address}");
        }
        for address in [
            "",
            "--upload-pack=command",
            "repo",
            "ext::command",
            "https://token@host/repo",
            "https://host/repo?token=x",
            "ssh://git:password@host/repo",
            "ftp://host/repo",
            "https://host/repo\n",
        ] {
            assert!(request(address).validate().is_err(), "{address}");
        }
        let mut value = request("/local/repo");
        value.id = "../other".into();
        assert!(value.validate().is_err());
        value.id = uuid::Uuid::nil().to_string();
        value.destination = "relative".into();
        assert!(value.validate().is_err());
    }
}
