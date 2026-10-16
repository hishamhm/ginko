use ginko::dts::{
    AnyDirective, FileType, HasSpan, IncludeLoader, IncludeLoaderGuard, IncludeLoaderNotifyAction,
    ItemAtCursor, Node, NodeItem, NodePayload, ParserConfig, Primary, Project, ResolvedContext,
    ResolvedNode, Severity, SeverityMap, Span,
};
use itertools::Itertools;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::request::GotoImplementationParams;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use url::Url;

pub struct Backend {
    name: String,
    client: Client,
    project: RwLock<Project>,
    severities: SeverityMap,
    loader: RwLock<IncludeLoaderGuard>,
}

impl Backend {
    pub fn new(name: &str, client: Client) -> Backend {
        Backend {
            name: name.to_string(),
            client,
            project: RwLock::new(Project::default()),
            severities: SeverityMap::default(),
            loader: RwLock::new(IncludeLoaderGuard::default()),
        }
    }
}

/// This is the shape of the JSON object we receive from the client
/// in the `did_change_configuration` callback. This is a free-form
/// object in LSP; we're choosing to receive the root DTS file here.
#[derive(Deserialize, Serialize, Default, Debug)]
struct ProjectConfig {
    pub target: String,
}

impl ProjectConfig {
    pub fn from_value(value: Value) -> Self {
        serde_json::from_value(value).unwrap_or_default()
    }
}

fn lsp_severity_from_severity(severity_level: Severity) -> DiagnosticSeverity {
    match severity_level {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Hint => DiagnosticSeverity::HINT,
    }
}

fn lsp_range_from_span(span: Span) -> Range {
    Range::new(lsp_pos_from_pos(span.start()), lsp_pos_from_pos(span.end()))
}

fn lsp_pos_from_pos(pos: ginko::dts::Position) -> Position {
    Position::new(pos.line(), pos.character())
}

impl Backend {
    pub fn with_loader<L>(mut self, loader: L) -> Self
    where
        L: IncludeLoader + Send + 'static,
    {
        self.loader = RwLock::new(IncludeLoaderGuard::new(loader));
        self
    }

    pub fn with_parser_config(self, config: ParserConfig) -> Self {
        self.project.write().set_parser_config(config);
        self
    }

    pub fn with_canonicalizer(self, f: fn(&Path) -> std::io::Result<PathBuf>) -> Self {
        self.project.write().set_canonicalizer(f);
        self
    }

    fn lsp_diag_from_diag(&self, diagnostic: &ginko::dts::Diagnostic) -> Diagnostic {
        let span = diagnostic.span();
        Diagnostic {
            range: lsp_range_from_span(span),
            message: diagnostic.message.clone(),
            code: Some(NumberOrString::String(diagnostic.kind.as_ref().to_string())),
            severity: Some(lsp_severity_from_severity(
                diagnostic.severity(&self.severities),
            )),
            source: Some(self.name.clone()),
            ..Default::default()
        }
    }

    async fn url_to_file_path(&self, url: &Url) -> Option<PathBuf> {
        match url.to_file_path() {
            Ok(path) => Some(path),
            Err(_) => {
                self.client
                    .show_message(
                        MessageType::ERROR,
                        format!("Url {url} is not a valid file path"),
                    )
                    .await;
                None
            }
        }
    }

    async fn publish_diagnostics(&self) {
        let file_paths = self
            .project
            .read()
            .files()
            .map(|file| file.to_owned())
            .collect_vec();
        for file in file_paths {
            let diagnostics = self
                .project
                .read()
                .get_diagnostics(&file)
                .map(|diag| self.lsp_diag_from_diag(diag))
                .collect_vec();
            self.client
                .publish_diagnostics(Url::from_file_path(&file).unwrap(), diagnostics, None)
                .await
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        // If we wanted, we could pass arbitrary data from the client
        // into this function as params.initialization_options.
        // But we are getting a `did_change_configuration` on start,
        // so we can handle the initial configuration from there.

        Ok(InitializeResult {
            server_info: None,
            capabilities: ServerCapabilities {
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec!["&".to_string(), "/".to_string()]),
                    all_commit_characters: None,
                    work_done_progress_options: WorkDoneProgressOptions {
                        work_done_progress: Some(true),
                    },
                    completion_item: None,
                }),
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
                }),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut changed = false;
        for change in params.changes {
            let Some(file_path) = self.url_to_file_path(&change.uri).await else {
                continue;
            };

            let mut loader = self.loader.write();
            match loader.notify(Path::new(file_path.as_path())) {
                IncludeLoaderNotifyAction::None => {}
                IncludeLoaderNotifyAction::Reset => {
                    let _ = self.project.write().reset(&mut loader);
                    changed = true;
                }
            }
        }
        if changed {
            self.publish_diagnostics().await;
            if let Err(err) = self.client.workspace_diagnostic_refresh().await {
                eprintln!("Error reporting workspace diagnostic refresh: {err}");
            }
        }
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let config = ProjectConfig::from_value(params.settings);
        {
            let mut loader = self.loader.write();
            let _ = self
                .project
                .write()
                .set_root_file_and_reset(config.target, &mut loader);
        }
        self.publish_diagnostics().await;
    }

    async fn initialized(&self, _: InitializedParams) {
        let Some(watchers) = self.loader.read().watch_patterns().map(|patterns| {
            patterns
                .into_iter()
                .map(|pattern| FileSystemWatcher {
                    glob_pattern: GlobPattern::String(pattern),
                    kind: None, // default: Create | Change | Delete
                })
                .collect::<Vec<_>>()
        }) else {
            return;
        };

        let _ = self
            .client
            .register_capability(vec![Registration {
                id: "watch-patterns".to_string(),
                method: "workspace/didChangeWatchedFiles".to_string(),
                register_options: serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                    watchers,
                })
                .ok(),
            }])
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let Some(file_path) = self.url_to_file_path(&params.text_document.uri).await else {
            return;
        };
        let file_type = FileType::from(file_path.as_path());
        self.project.write().add_file_with_text(
            file_path.clone(),
            params.text_document.text,
            file_type,
            &mut self.loader.write(),
        );
        self.publish_diagnostics().await
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(file_path) = self.url_to_file_path(&params.text_document.uri).await else {
            return;
        };
        let file_type = FileType::from(file_path.as_path());
        self.project.write().add_file_with_text(
            file_path.clone(),
            params.content_changes.into_iter().next().unwrap().text,
            file_type,
            &mut self.loader.write(),
        );
        self.publish_diagnostics().await
    }

    async fn did_save(&self, _: DidSaveTextDocumentParams) {}

    async fn did_close(&self, _: DidCloseTextDocumentParams) {}

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        fn completions_for_labels(
            labels: Vec<(String, Option<String>)>,
            token: &str,
        ) -> Option<CompletionResponse> {
            Some(CompletionResponse::Array(
                labels
                    .into_iter()
                    .filter(|(label, _)| label.starts_with(token))
                    .map(|(label, detail)| CompletionItem {
                        label,
                        detail,
                        ..Default::default()
                    })
                    .collect(),
            ))
        }

        fn completions_for_reference(
            project: &Project,
            file_path: &Path,
            token: &str,
        ) -> Option<CompletionResponse> {
            let analysis = project.get_analysis(file_path)?;
            let labels = analysis
                .get_labels()
                .into_iter()
                .filter(|label| label.starts_with(token))
                .map(|label| {
                    let detail = analysis.get_referred_by_label(&label);
                    (label, detail)
                })
                .collect();
            completions_for_labels(labels, token)
        }

        fn completions_for_referenced_node(
            project: &Project,
            file_path: &Path,
            token: &str,
        ) -> Option<CompletionResponse> {
            let analysis = project.get_analysis(file_path)?;
            Some(CompletionResponse::Array(
                analysis
                    .get_labels()
                    .into_iter()
                    .filter(|label| label.starts_with(token))
                    .filter_map(|label| {
                        if let Some(detail) = analysis.get_referred_by_label(&label) {
                            let snippet = format!("{label} {{\n\t$0\n}};");
                            Some(CompletionItem {
                                label,
                                detail: Some(detail),
                                kind: Some(CompletionItemKind::MODULE),
                                insert_text: Some(snippet),
                                insert_text_format: Some(InsertTextFormat::SNIPPET),
                                ..Default::default()
                            })
                        } else {
                            None
                        }
                    })
                    .collect(),
            ))
        }

        fn completions_for_node(rnode: &ResolvedNode) -> Option<CompletionResponse> {
            let mut items = vec![];
            for name in rnode.active_child_nodes() {
                let snippet = format!("{name} {{\n\t$0\n}};");
                items.push(CompletionItem {
                    label: name,
                    detail: Some("node".to_string()),
                    kind: Some(CompletionItemKind::MODULE),
                    insert_text: Some(snippet),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
            for (name, prop) in rnode.active_child_properties() {
                let snippet = if prop.values.is_empty() {
                    format!("{name};")
                } else {
                    format!("{name} = $0{};", prop.display_values())
                };
                items.push(CompletionItem {
                    label: name,
                    detail: Some("property".to_string()),
                    insert_text: Some(snippet),
                    insert_text_format: Some(InsertTextFormat::SNIPPET),
                    ..Default::default()
                });
            }
            Some(CompletionResponse::Array(items))
        }

        fn completions_for_root() -> Option<CompletionResponse> {
            let mut items = vec![];
            let snippet = " {\n\t$0\n};".to_string();
            items.push(CompletionItem {
                label: "/".to_string(),
                detail: Some("node".to_string()),
                kind: Some(CompletionItemKind::MODULE),
                insert_text: Some(snippet),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
            Some(CompletionResponse::Array(items))
        }

        let Some(file_path) = self
            .url_to_file_path(&params.text_document_position.text_document.uri)
            .await
        else {
            return Ok(None);
        };

        let pos = position_to_ginko_position(params.text_document_position.position);
        let project = self.project.read();
        let Some(token) = project.token_at(&file_path, &pos) else {
            return Ok(None);
        };

        // Check if cursor is on a "node context" (e.g. inside node brackets),
        // or not (e.g. at the toplevel).
        match project.context_at(&file_path, &pos) {
            Some(ResolvedContext::Node(rnode, _)) => {
                if let Some(token) = token.strip_prefix('&') {
                    return Ok(completions_for_reference(&project, &file_path, token));
                }

                return Ok(completions_for_node(rnode));
            }
            Some(ResolvedContext::Property(_, ppath)) => {
                if let Some(token) = token.strip_prefix('&') {
                    if let Some(labels) = project.valid_references_for_property(&file_path, &ppath)
                    {
                        return Ok(completions_for_labels(labels, token));
                    }
                    return Ok(completions_for_reference(&project, &file_path, token));
                }
            }
            None => {
                if let Some(token) = token.strip_prefix('&') {
                    return Ok(completions_for_referenced_node(&project, &file_path, token));
                }

                if let Some(context) = params.context {
                    if context.trigger_kind == CompletionTriggerKind::TRIGGER_CHARACTER
                        && context.trigger_character == Some("/".to_string())
                        && params.text_document_position.position.character == 1
                    {
                        return Ok(completions_for_root());
                    }
                }
            }
        }

        Ok(None)
    }

    async fn goto_implementation(
        &self,
        params: GotoImplementationParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let Some(file_path) = self
            .url_to_file_path(&params.text_document_position_params.text_document.uri)
            .await
        else {
            return Ok(None);
        };
        let pos = position_to_ginko_position(params.text_document_position_params.position);
        let project = self.project.read();

        match project.context_at(&file_path, &pos) {
            Some(ResolvedContext::Node(rnode, _)) => {
                return Ok(Some(GotoDefinitionResponse::Array(
                    rnode
                        .sources()
                        .map(|loc| {
                            Location::new(
                                Url::from_file_path(loc.file()).unwrap(),
                                ginko_span_to_range(loc.span()),
                            )
                        })
                        .collect(),
                )));
            }
            Some(ResolvedContext::Property(_, _)) => Ok(None),
            None => Ok(None),
        }
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let Some(file_path) = self
            .url_to_file_path(&params.text_document_position_params.text_document.uri)
            .await
        else {
            return Ok(None);
        };
        let pos = position_to_ginko_position(params.text_document_position_params.position);
        let project = self.project.read();
        let Some(item) = project.find_at_pos(&file_path, &pos) else {
            return Ok(None);
        };
        match item {
            ItemAtCursor::Reference(reference, ctx) => {
                match project.get_node_position(&file_path, reference, &ctx) {
                    Some((span, path)) => Ok(Some(GotoDefinitionResponse::Scalar(Location::new(
                        Url::from_file_path(path).unwrap(),
                        ginko_span_to_range(span),
                    )))),
                    None => Ok(None),
                }
            }
            ItemAtCursor::Include(include) => {
                match self
                    .loader
                    .write()
                    .load(&file_path, &include.file_name())
                    .ok()
                    .and_then(|path| Url::from_file_path(path).ok())
                {
                    Some(url) => Ok(Some(GotoDefinitionResponse::Scalar(Location::new(
                        url,
                        ginko_span_to_range(include.span()),
                    )))),
                    _ => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let Some(file_path) = self
            .url_to_file_path(&params.text_document_position_params.text_document.uri)
            .await
        else {
            return Ok(None);
        };
        let pos = position_to_ginko_position(params.text_document_position_params.position);
        let project = self.project.read();
        let Some(item) = project.find_at_pos(&file_path, &pos) else {
            return Ok(None);
        };

        let Some(str) = (match item {
            ItemAtCursor::Node(node) if node.span().start().line() == pos.line() => {
                context_based_hover(&project, &file_path, &pos)
            }
            ItemAtCursor::ReferencedNode(rnode) if rnode.span().start().line() == pos.line() => {
                context_based_hover(&project, &file_path, &pos)
            }
            ItemAtCursor::Reference(reference, ctx) => {
                project.document_reference(&file_path, reference, &ctx)
            }
            ItemAtCursor::Label(name) => Some(name.item().clone()),
            ItemAtCursor::Include(include) => Some(include.file_name.item().clone()),
            _ => None,
        }) else {
            return Ok(None);
        };

        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::LanguageString(LanguageString {
                language: "dts".to_string(),
                value: str.to_string(),
            })),
            range: None,
        }))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let Some(file_path) = self.url_to_file_path(&params.text_document.uri).await else {
            return Ok(None);
        };
        let project = self.project.read();
        let Some(root) = project.get_root(&file_path) else {
            return Ok(None);
        };
        #[allow(deprecated)]
        fn node_payload_to_symbol(payload: &NodePayload) -> Vec<DocumentSymbol> {
            let mut children: Vec<DocumentSymbol> = Vec::new();
            for el in &payload.items {
                let document_symbol = match el {
                    NodeItem::Property(prop) => DocumentSymbol {
                        name: prop.name.item().clone(),
                        detail: None,
                        kind: SymbolKind::PROPERTY,
                        tags: None,
                        deprecated: None,
                        range: ginko_span_to_range(prop.span()),
                        selection_range: ginko_span_to_range(prop.name.span()),
                        children: None,
                    },
                    NodeItem::Node(node) => node_to_symbol(node),
                    NodeItem::DeletedNode(start_tok, deleted_node) => DocumentSymbol {
                        name: format!("{}", deleted_node.item()),
                        detail: None,
                        kind: SymbolKind::MODULE,
                        tags: Some(vec![SymbolTag::DEPRECATED]),
                        deprecated: None,
                        range: ginko_span_to_range(Span::new(
                            start_tok.start(),
                            deleted_node.end(),
                        )),
                        selection_range: ginko_span_to_range(deleted_node.span()),
                        children: None,
                    },
                    NodeItem::DeletedProperty(start_tok, deleted_property) => DocumentSymbol {
                        name: deleted_property.item().to_string(),
                        detail: None,
                        kind: SymbolKind::PROPERTY,
                        tags: Some(vec![SymbolTag::DEPRECATED]),
                        deprecated: None,
                        range: ginko_span_to_range(Span::new(
                            start_tok.start(),
                            deleted_property.end(),
                        )),
                        selection_range: ginko_span_to_range(deleted_property.span()),
                        children: None,
                    },
                };
                children.push(document_symbol);
            }
            children
        }
        #[allow(deprecated)]
        fn node_to_symbol(node: &Node) -> DocumentSymbol {
            DocumentSymbol {
                name: node.name.name.clone(),
                detail: None,
                kind: SymbolKind::MODULE,
                tags: None,
                deprecated: None,
                range: ginko_span_to_range(node.span()),
                selection_range: ginko_span_to_range(node.name.span()),
                children: Some(node_payload_to_symbol(&node.payload)),
            }
        }
        #[allow(deprecated)]
        let nodes = root
            .elements
            .iter()
            .filter_map(|el| match el {
                Primary::Root(node) => Some(node_to_symbol(node)),
                Primary::ReferencedNode(ref_node) => Some(DocumentSymbol {
                    name: format!("{}", ref_node.reference),
                    detail: None,
                    kind: SymbolKind::MODULE,
                    tags: None,
                    deprecated: None,
                    range: ginko_span_to_range(ref_node.span()),
                    selection_range: ginko_span_to_range(ref_node.reference.span()),
                    children: Some(node_payload_to_symbol(&ref_node.payload)),
                }),
                Primary::Directive(AnyDirective::Include(include)) => Some(DocumentSymbol {
                    name: format!("include {}", include.file_name.item()),
                    detail: None,
                    kind: SymbolKind::FILE,
                    tags: None,
                    deprecated: None,
                    range: ginko_span_to_range(include.span()),
                    selection_range: ginko_span_to_range(include.file_name.span()),
                    children: None,
                }),
                Primary::Directive(AnyDirective::DeletedNode(token, ref_node)) => {
                    Some(DocumentSymbol {
                        name: format!("{}", ref_node),
                        detail: None,
                        kind: SymbolKind::MODULE,
                        tags: Some(vec![SymbolTag::DEPRECATED]),
                        deprecated: None,
                        range: ginko_span_to_range(Span::new(token.start(), ref_node.end())),
                        selection_range: ginko_span_to_range(ref_node.span()),
                        children: None,
                    })
                }
                Primary::Directive(AnyDirective::OmitIfNoRef(token, ref_node)) => {
                    Some(DocumentSymbol {
                        name: format!("{}", ref_node),
                        detail: None,
                        kind: SymbolKind::FIELD,
                        tags: None,
                        deprecated: None,
                        range: ginko_span_to_range(Span::new(token.start(), ref_node.end())),
                        selection_range: ginko_span_to_range(ref_node.span()),
                        children: None,
                    })
                }
                _ => None,
            })
            .collect_vec();
        Ok(Some(DocumentSymbolResponse::Nested(nodes)))
    }
}

fn ginko_span_to_range(span: Span) -> Range {
    Range::new(
        ginko_position_to_position(span.start()),
        ginko_position_to_position(span.end()),
    )
}

#[allow(unused)]
fn range_to_ginko_span(range: Range) -> Span {
    Span::new(
        position_to_ginko_position(range.start),
        position_to_ginko_position(range.end),
    )
}

fn position_to_ginko_position(position: Position) -> ginko::dts::Position {
    ginko::dts::Position::new(position.line, position.character)
}

fn ginko_position_to_position(position: ginko::dts::Position) -> Position {
    Position::new(position.line(), position.character())
}

fn context_based_hover(
    project: &Project,
    file_path: &Path,
    pos: &ginko::dts::Position,
) -> Option<String> {
    match project.context_at(file_path, pos) {
        Some(ResolvedContext::Node(rnode, _)) => Some(rnode.to_string()),
        Some(ResolvedContext::Property(_, _)) => None,
        None => None,
    }
}
