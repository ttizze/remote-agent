//! Ownership and ancestry of filesystem checkpoints; no filesystem or provider I/O.
use crate::{decider::DecisionError, *};
use std::{
    collections::BTreeSet,
    path::{Component, Path},
};

pub fn contains(
    scopes: &[CheckpointScope],
    ancestor: &CheckpointScopeId,
    descendant: &CheckpointScopeId,
) -> bool {
    let mut current = Some(descendant);
    let mut visited = BTreeSet::new();
    for _ in 0..128 {
        let Some(id) = current else { return false };
        if id == ancestor {
            return true;
        }
        if !visited.insert(id) {
            return false;
        }
        current = scopes
            .iter()
            .find(|s| &s.id == id)
            .and_then(|s| s.parent_scope_id.as_ref());
    }
    false
}

pub fn owns_node(nodes: &[ExecutionNode], root: &NodeId, node: &NodeId) -> bool {
    let mut current = Some(node);
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..128 {
        let Some(id) = current else { return false };
        if id == root {
            return true;
        }
        if !seen.insert(id) {
            return false;
        }
        current = nodes
            .iter()
            .find(|n| &n.id == id)
            .and_then(|n| n.parent_node_id.as_ref());
    }
    false
}

pub fn validate(
    scope: &CheckpointScope,
    thread: &ThreadId,
    nodes: &[ExecutionNode],
    runs: &[Run],
    providers: &[ProviderThread],
    scopes: &[CheckpointScope],
) -> Result<(), DecisionError> {
    let invalid = || DecisionError("checkpoint scope ownership is invalid".into());
    let cwd = Path::new(&scope.cwd);
    if scope.thread_id != *thread
        || !cwd.is_absolute()
        || cwd
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || scope.advances_app_run_count != (scope.kind == ScopeKind::RootRun)
    {
        return Err(invalid());
    }
    let node = nodes
        .iter()
        .find(|n| n.id == scope.node_id && n.thread_id == *thread)
        .ok_or_else(invalid)?;
    if node.run_id != scope.run_id
        || node.status == NodeStatus::RolledBack
        || node.provider_thread_id != scope.provider_thread_id
        || scope.provider_thread_id.as_ref().is_some_and(|id| {
            !providers
                .iter()
                .any(|p| &p.id == id && p.app_thread_id.as_ref() == Some(thread))
        })
    {
        return Err(invalid());
    }
    if let Some(run_id) = &scope.run_id {
        let run = runs
            .iter()
            .find(|r| &r.id == run_id && r.status != RunStatus::RolledBack)
            .ok_or_else(invalid)?;
        if scope.kind == ScopeKind::RootRun && run.root_node_id.as_ref() != Some(&scope.node_id) {
            return Err(invalid());
        }
    }
    if let Some(parent_id) = &scope.parent_scope_id {
        let parent = scopes
            .iter()
            .find(|s| &s.id == parent_id && s.thread_id == *thread)
            .ok_or_else(invalid)?;
        if scope.kind == ScopeKind::RootRun
            || parent.id == scope.id
            || contains(scopes, &scope.id, parent_id)
            || !cwd.starts_with(&parent.cwd)
        {
            return Err(invalid());
        }
        if !owns_node(nodes, &parent.node_id, &node.id) {
            return Err(invalid());
        }
    }
    if let Some(existing) = scopes.iter().find(|s| s.id == scope.id)
        && (existing.thread_id != scope.thread_id
            || existing.cwd != scope.cwd && scope.kind != ScopeKind::RootRun
            || existing.kind != scope.kind
            || existing.parent_scope_id != scope.parent_scope_id
            || existing.ordinal_within_parent != scope.ordinal_within_parent
            || scope.kind != ScopeKind::RootRun
                && (existing.run_id != scope.run_id
                    || existing.node_id != scope.node_id
                    || existing.provider_thread_id != scope.provider_thread_id))
    {
        return Err(DecisionError(
            "checkpoint scope identity cannot be changed".into(),
        ));
    }
    Ok(())
}

pub fn capture_owned(
    capture: &CheckpointCapture,
    scope: &CheckpointScope,
    runs: &[Run],
    nodes: &[ExecutionNode],
    checkpoints: &[Checkpoint],
) -> bool {
    capture.scope_id == scope.id
        && capture.run_id == scope.run_id
        && capture.node_id == scope.node_id
        && capture.ordinal_within_scope > 0
        && capture.app_run_ordinal.is_none()
        && !scope.advances_app_run_count
        && (capture.run_id.is_some() || capture.attempt_id.is_none())
        && nodes
            .iter()
            .any(|n| n.id == capture.node_id && n.status != NodeStatus::RolledBack)
        && capture.run_id.as_ref().is_none_or(|id| {
            runs.iter().any(|r| {
                &r.id == id
                    && r.status != RunStatus::RolledBack
                    && r.active_attempt_id == capture.attempt_id
            })
        })
        && capture.parent_checkpoint_id.as_ref().is_some_and(|id| {
            checkpoints.iter().any(|c| {
                &c.id == id
                    && c.scope_id == scope.id
                    && c.status == CheckpointStatus::Ready
                    && c.ordinal_within_scope < capture.ordinal_within_scope
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use proptest::prelude::*;
    #[test]
    fn nested_scope_rejects_escape_cycles_and_unrelated_nodes() {
        let mut p = running();
        let root = checkpoint_scope(&p.runs[0]);
        let mut node = p.nodes[0].clone();
        node.id = NodeId::new("child").unwrap();
        node.parent_node_id = Some(root.node_id.clone());
        node.kind = NodeKind::ToolCall;
        node.counts_for_run = false;
        p.nodes.push(node.clone());
        let scope = CheckpointScope {
            id: CheckpointScopeId::new("nested").unwrap(),
            node_id: node.id.clone(),
            parent_scope_id: Some(root.id.clone()),
            kind: ScopeKind::Tool,
            advances_app_run_count: false,
            cwd: "/workspace/nested".into(),
            ..root.clone()
        };
        let check =
            |scope: &CheckpointScope, nodes: &[ExecutionNode], scopes: &[CheckpointScope]| {
                validate(
                    scope,
                    &p.thread.id,
                    nodes,
                    &p.runs,
                    &p.provider_threads,
                    scopes,
                )
            };
        assert!(check(&scope, &p.nodes, std::slice::from_ref(&root)).is_ok());
        for cwd in ["/workspace/../outside", "relative", "/workspace-sibling"] {
            assert!(
                check(
                    &CheckpointScope {
                        cwd: cwd.into(),
                        ..scope.clone()
                    },
                    &p.nodes,
                    std::slice::from_ref(&root)
                )
                .is_err()
            );
        }
        let mut unrelated = p.nodes.clone();
        unrelated[1].parent_node_id = None;
        assert!(check(&scope, &unrelated, std::slice::from_ref(&root)).is_err());
        let cyclic = CheckpointScope {
            parent_scope_id: Some(scope.id.clone()),
            ..root.clone()
        };
        assert!(check(&scope, &p.nodes, &[cyclic]).is_err());
        assert!(
            check(
                &CheckpointScope {
                    advances_app_run_count: true,
                    ..scope
                },
                &p.nodes,
                &[root]
            )
            .is_err()
        );
    }
    proptest! {
        #[test]
        fn bounded_scope_ancestry_handles_cycles(count in 1usize..128, loop_back in any::<bool>()) {
            let p=running(); let template=checkpoint_scope(&p.runs[0]);
            let scopes:Vec<_>=(0..count).map(|i|CheckpointScope {
                id:CheckpointScopeId::new(format!("scope:{i}")).unwrap(),
                parent_scope_id:if i>0 {Some(CheckpointScopeId::new(format!("scope:{}",i-1)).unwrap())} else if loop_back {Some(CheckpointScopeId::new(format!("scope:{}",count-1)).unwrap())} else {None},
                ..template.clone()
            }).collect();
            prop_assert!(contains(&scopes,&scopes[0].id,&scopes[count-1].id));
            prop_assert!(!contains(&scopes,&CheckpointScopeId::new("foreign").unwrap(),&scopes[count-1].id));
        }
    }
}
