use super::{intents::Next, owner::Owner};
use crate::{peer::PeerError, protocol::Call, state::Intent};
use agent_protocol::vcs::{
    CloneProtocol, PreparePullRequestThread, PublishRepository, PullRequestThreadMode,
    RepositoryVisibility, RunStackedAction, SourceControlProviderKind, StackedAction,
};

fn action(value: &str) -> Result<StackedAction, PeerError> {
    match value {
        "commit" => Ok(StackedAction::Commit),
        "push" => Ok(StackedAction::Push),
        "create_pr" => Ok(StackedAction::CreatePr),
        "commit_push" => Ok(StackedAction::CommitPush),
        "commit_push_pr" => Ok(StackedAction::CommitPushPr),
        _ => Err(super::intents::invalid("Unknown Git action")),
    }
}

fn thread_mode(value: &str) -> Result<PullRequestThreadMode, PeerError> {
    match value {
        "local" => Ok(PullRequestThreadMode::Local),
        "worktree" => Ok(PullRequestThreadMode::Worktree),
        _ => Err(super::intents::invalid(
            "Unknown pull request checkout mode",
        )),
    }
}

fn parse_visibility(value: &str) -> Result<RepositoryVisibility, PeerError> {
    match value {
        "private" => Ok(RepositoryVisibility::Private),
        "public" => Ok(RepositoryVisibility::Public),
        _ => Err(super::intents::invalid("Unknown repository visibility")),
    }
}

fn parse_protocol(value: Option<String>) -> Result<Option<CloneProtocol>, PeerError> {
    value
        .map(|value| match value.as_str() {
            "auto" => Ok(CloneProtocol::Auto),
            "ssh" => Ok(CloneProtocol::Ssh),
            "https" => Ok(CloneProtocol::Https),
            _ => Err(super::intents::invalid("Unknown clone protocol")),
        })
        .transpose()
}

impl Owner {
    pub(super) fn prepare_vcs_intent(&mut self, intent: Intent) -> Result<Next, PeerError> {
        match intent {
            Intent::RefreshVcsStatus { cwd } => Ok(Next::call(
                Call::RefreshVcsStatus(agent_protocol::vcs::RefreshVcsStatus { cwd }),
                None,
            )),
            Intent::LoadVcsRefs { cwd, query } => {
                self.load_refs(cwd, crate::state::RefScope::All, query);
                Ok(Next::Done)
            }
            Intent::SwitchVcsRef { cwd, ref_name } => Ok(Next::call(
                Call::SwitchRef(agent_protocol::workspace::SwitchRef { cwd, ref_name }),
                None,
            )),
            Intent::CreateVcsRef { cwd, ref_name } => Ok(Next::call(
                Call::CreateRef(agent_protocol::workspace::CreateRef {
                    cwd,
                    ref_name,
                    switch_ref: true,
                }),
                None,
            )),
            Intent::PullVcs { cwd } => Ok(Next::call(
                Call::Pull(agent_protocol::vcs::Pull { cwd }),
                None,
            )),
            Intent::RunVcsAction {
                action_id,
                cwd,
                action: name,
                commit_message,
                feature_branch,
                file_paths,
                thread_id,
                project_id,
            } => {
                let request = RunStackedAction {
                    action_id,
                    cwd,
                    action: action(&name)?,
                    commit_message,
                    feature_branch,
                    file_paths,
                    thread_id: thread_id
                        .map(agent_domain::ThreadId::new)
                        .transpose()
                        .map_err(super::intents::invalid)?,
                    project_id,
                };
                self.start_git_action(request);
                Ok(Next::Done)
            }
            Intent::InitRepository { cwd } => Ok(Next::call(
                Call::InitRepository(agent_protocol::vcs::InitRepository { cwd }),
                None,
            )),
            Intent::CreateVcsWorktree {
                cwd,
                ref_name,
                new_ref_name,
                base_ref_name,
                path,
            } => Ok(Next::call(
                Call::CreateWorktree(agent_protocol::vcs::CreateWorktree {
                    cwd,
                    ref_name,
                    new_ref_name,
                    base_ref_name,
                    path,
                }),
                None,
            )),
            Intent::RemoveVcsWorktree { cwd, path, force } => Ok(Next::call(
                Call::RemoveWorktreeCheckout(agent_protocol::vcs::RemoveWorktreeCheckout {
                    cwd,
                    path,
                    force,
                }),
                None,
            )),
            Intent::ResolvePullRequest { cwd, reference } => Ok(Next::call(
                Call::ResolvePullRequest(agent_protocol::vcs::ResolvePullRequest {
                    cwd,
                    reference,
                }),
                None,
            )),
            Intent::PreparePullRequestThread {
                cwd,
                reference,
                mode,
                thread_id,
            } => Ok(Next::call(
                Call::PreparePullRequestThread(PreparePullRequestThread {
                    cwd,
                    reference,
                    mode: thread_mode(&mode)?,
                    thread_id: thread_id
                        .map(agent_domain::ThreadId::new)
                        .transpose()
                        .map_err(super::intents::invalid)?,
                }),
                None,
            )),
            Intent::PublishRepository {
                cwd,
                repository,
                visibility,
                remote_name,
                protocol,
            } => Ok(Next::call(
                Call::PublishRepository(PublishRepository {
                    cwd,
                    provider: SourceControlProviderKind::Github,
                    repository,
                    visibility: parse_visibility(&visibility)?,
                    remote_name,
                    protocol: parse_protocol(protocol)?,
                }),
                None,
            )),
            _ => Err(super::intents::invalid("unexpected Git intent")),
        }
    }
}
