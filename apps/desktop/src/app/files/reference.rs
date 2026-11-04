use super::menu::FileMenuTarget;
use super::*;
use agent_core::presentation::{
    editors::preferred_editor,
    files::{FileAction, FilePrimaryAction, desktop_file_primary_action, file_reference_menu},
    markdown::{MarkdownFileReference, file_workspace_relative_path},
};

pub(in crate::app) enum ReferenceAction {
    Open(u64),
    Menu(FileAction),
}
pub(in crate::app) struct ReferenceRequest {
    pub epoch: u64,
    pub target: FileMenuTarget,
    pub action: ReferenceAction,
}

impl Desktop {
    pub(in crate::app) fn reference_target(
        &self,
        file: &MarkdownFileReference,
        lookup_path: Option<&str>,
    ) -> FileMenuTarget {
        let root = agent_core::presentation::markdown::file_host_path(
            self.snapshot.navigation.cwd.clone(),
            self.snapshot.host_home_directory.clone(),
        );
        FileMenuTarget {
            path: file.path.clone(),
            relative: file_workspace_relative_path(&file.path, &root),
            line: file.line,
            column: file.column,
            mention: String::new(),
            resolve_reference: true,
            lookup_path: lookup_path.map(str::to_owned),
            root,
        }
    }
    pub(in crate::app) fn show_reference_menu(
        &self,
        file: &MarkdownFileReference,
        lookup_path: Option<&str>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let capabilities = self.snapshot.file_open_capabilities.as_deref();
        let target = self.reference_target(file, lookup_path);
        let editor = preferred_editor(
            self.snapshot.preferred_editor,
            capabilities.map_or(&[], |value| value.editors.as_slice()),
        );
        let items = file_reference_menu(
            self.local_shell,
            editor,
            capabilities.and_then(|value| value.reveal),
            target.relative.is_some(),
        );
        self.show_file_menu(items, target, position, window, cx);
    }
    pub(in crate::app) fn activate_reference(
        &mut self,
        file: &MarkdownFileReference,
        lookup_path: Option<&str>,
        modifier: bool,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = self.reference_target(file, lookup_path);
        let action = desktop_file_primary_action(
            &file.path,
            &target.root,
            self.selected().is_some(),
            self.local_shell,
            modifier,
        );
        match action {
            FilePrimaryAction::Open => self.open_reference(target, window, cx),
            FilePrimaryAction::Editor => self.activate_file_action(
                &target,
                FileAction::OpenEditor { editor: None },
                window,
                cx,
            ),
            FilePrimaryAction::Options => {
                self.show_reference_menu(file, lookup_path, position, window, cx)
            }
        }
    }
    pub(in crate::app) fn open_reference(
        &mut self,
        target: FileMenuTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_open_generation += 1;
        if !self.snapshot.connected {
            let directory = self
                .snapshot
                .file_metadata(target.path.clone())
                .and_then(|entry| entry.metadata)
                .is_some_and(|metadata| {
                    metadata.kind == agent_protocol::file_metadata::FileMetadataKind::Directory
                });
            self.open_resolved_reference(target.path, directory, target.line, window, cx);
            return;
        }
        self.resolve_reference_action(target, ReferenceAction::Open(self.file_open_generation));
    }
    pub(super) fn resolve_reference_action(&self, target: FileMenuTarget, action: ReferenceAction) {
        self.perform(
            Intent::ResolveFileReference(op::ResolveFileReference {
                surface: agent_core::presentation::markdown::MarkdownFileSurface::Desktop,
                path: target.path.clone(),
                root: target.root.clone(),
                lookup_path: target.lookup_path.clone(),
                inspect_directory: matches!(action, ReferenceAction::Open(_)),
            }),
            OperationCompletion::FileReference(Box::new(ReferenceRequest {
                epoch: self.snapshot.epoch,
                target,
                action,
            })),
        );
    }
    pub(in crate::app) fn complete_reference(
        &mut self,
        request: ReferenceRequest,
        result: Result<Outcome, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if request.epoch != self.snapshot.epoch
            || matches!(request.action, ReferenceAction::Open(id) if id != self.file_open_generation)
        {
            return;
        }
        match result {
            Ok(Outcome::FileReferenceResolved { path, directory }) => {
                let mut target = request.target;
                target.path = path;
                target.relative = file_workspace_relative_path(&target.path, &target.root);
                match request.action {
                    ReferenceAction::Open(_) => self.open_resolved_reference(
                        target.path,
                        directory,
                        target.line,
                        window,
                        cx,
                    ),
                    ReferenceAction::Menu(action) => {
                        self.finish_file_action(&target, action, window, cx)
                    }
                }
            }
            Err(error) => {
                self.set_error(error);
                cx.notify();
            }
            _ => {}
        }
    }
    fn open_resolved_reference(
        &mut self,
        path: String,
        directory: bool,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if directory {
            self.browse(path, cx);
        } else {
            self.reveal_file_in_tree(Some(&path), false);
            self.open_file(path, line, false, cx);
            self.sync_inputs(window, cx);
            cx.notify();
        }
    }
}
