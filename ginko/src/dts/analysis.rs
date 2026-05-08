use itertools::Itertools;

use crate::dts::ast::{
    AnyDirective, Cell, DtsFile, Include, Node, NodeItem, NodePayload, Path, Primary, Property,
    PropertyPath, PropertyValue, Reference, ReferencedNode, WithToken,
};
use crate::dts::data::{HasSource, HasSpan, Span};
use crate::dts::error_codes::ErrorCode;
use crate::dts::import_guard::ImportGuard;
use crate::dts::loader::IncludeLoaderGuard;
use crate::dts::{Diagnostic, FileType, Position, Project};
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

/// Something that can be labeled.
/// Used when analyzing a device-tree
#[derive(Clone, Debug)]
enum Labeled {
    Node(Arc<Node>),
    #[allow(unused)]
    Property(Arc<Property>),
    ReferencedNode(Arc<ReferencedNode>),
}

/// Struct containing all important information when analyzing a device-tree.
pub(crate) struct Analysis {
    import_guard: ImportGuard<PathBuf>,
    loader: IncludeLoaderGuard,
    context: AnalysisContext,
}

impl Analysis {
    pub fn new(
        include_cache: Option<HashMap<String, PathBuf>>,
        loader: &IncludeLoaderGuard,
    ) -> Analysis {
        let mut context = AnalysisContext::default();
        if let Some(include_cache) = include_cache {
            context.includes = include_cache;
        }

        Analysis {
            import_guard: ImportGuard::default(),
            loader: loader.clone(),
            context,
        }
    }

    pub fn into_context(self) -> AnalysisContext {
        self.context
    }
}

#[derive(Clone, Default)]
pub struct AnalysisContext {
    labels: HashMap<String, Labeled>,
    flat_nodes: HashMap<Path, Arc<Node>>,
    includes: HashMap<String, PathBuf>,
}

pub struct AnalysisResult {
    pub diagnostics: Vec<Diagnostic>,
}

impl AnalysisContext {
    pub fn get_includes(&self) -> &HashMap<String, PathBuf> {
        &self.includes
    }

    pub fn get_node_by_label(&self, label: &str) -> Option<&Arc<Node>> {
        match self.labels.get(label) {
            Some(Labeled::Node(node)) => Some(node),
            _ => None,
        }
    }

    pub fn get_node_by_path(&self, path: &Path) -> Option<&Arc<Node>> {
        self.flat_nodes.get(path)
    }

    pub fn get_referenced(&self, reference: &Reference) -> Option<&Arc<Node>> {
        match reference {
            Reference::Label(label) => self.get_node_by_label(label),
            Reference::Path(path) => self.get_node_by_path(path),
            Reference::PropertyPath(path) => self.get_node_by_path(path.node_path()),
        }
    }

    pub fn get_node_position(&self, node: &Arc<Node>) -> (Span, Arc<std::path::Path>) {
        (node.name.span(), node.name.source())
    }

    pub fn get_referenced_node_position(
        &self,
        node: &Arc<ReferencedNode>,
    ) -> Option<(Span, Arc<std::path::Path>)> {
        node.label
            .as_ref()
            .map(|label| (label.span(), label.source()))
    }

    fn get_property_by_property_path(&self, path: &PropertyPath) -> Option<Arc<Property>> {
        let node = self.get_node_by_path(path.node_path())?;
        let payload = &node.payload;
        let property_name = path.property_name();
        for item in &payload.items {
            if let NodeItem::Property(property) = item {
                if property.name.item() == property_name {
                    return Some(property.clone());
                }
            }
        }
        None
    }

    pub fn get_position(&self, reference: &Reference) -> Option<(Span, Arc<std::path::Path>)> {
        match reference {
            Reference::Label(label) => match self.labels.get(label) {
                Some(Labeled::Node(node)) => Some(self.get_node_position(node)),
                Some(Labeled::ReferencedNode(node)) => self.get_referenced_node_position(node),
                _ => None,
            },
            Reference::Path(path) => self
                .get_node_by_path(path)
                .map(|node| self.get_node_position(node)),
            Reference::PropertyPath(path) => {
                let property = self.get_property_by_property_path(path)?;
                Some((property.name.span(), property.name.source()))
            }
        }
    }

    pub fn get_referred(&self, reference: &Reference) -> Option<String> {
        match reference {
            Reference::Label(label) => match self.labels.get(label) {
                Some(Labeled::Node(node)) => {
                    let mut name = node.name.name.clone();
                    if let Some(unit_address) = &node.name.unit_address {
                        name += "@";
                        name += unit_address;
                    }
                    Some(name)
                }
                Some(Labeled::ReferencedNode(node)) => node.label.as_deref().cloned(),
                _ => None,
            },
            Reference::Path(path) => self
                .get_node_by_path(path)
                .map(|node| node.name.name.clone()),
            Reference::PropertyPath(path) => {
                let property = self.get_property_by_property_path(path)?;
                Some(property.values.iter().map(|v| v.to_string()).join(", "))
            }
        }
    }
}

pub struct FileContext<'a> {
    source: Arc<StdPath>,
    project: &'a Project,
    diagnostics: Vec<Diagnostic>,
    unresolved_references: Vec<WithToken<Reference>>,
    file_type: FileType,
    is_plugin: bool,
    first_non_include: bool,
    dts_header_seen: bool,
}

impl FileContext<'_> {
    pub fn add_diagnostic(&mut self, diagnostic: Diagnostic) {
        self.diagnostics.push(diagnostic);
    }
}

impl FileContext<'_> {
    pub fn into_result(self) -> AnalysisResult {
        AnalysisResult {
            diagnostics: self.diagnostics,
        }
    }
}

impl Analysis {
    pub fn analyze_file(
        &mut self,
        file: &DtsFile,
        file_type: FileType,
        project: &Project,
    ) -> AnalysisResult {
        let mut ctx = FileContext {
            source: file.source.clone(),
            file_type,
            diagnostics: Vec::default(),
            unresolved_references: Vec::default(),
            project,
            is_plugin: file_type == FileType::DtSourceOverlay,
            dts_header_seen: false,
            first_non_include: false,
        };
        for primary in &file.elements {
            match primary {
                Primary::Directive(directive) => match directive {
                    AnyDirective::DtsHeader(tok) => {
                        if ctx.dts_header_seen {
                            ctx.add_diagnostic(Diagnostic::from_token(
                                tok.clone(),
                                ErrorCode::DuplicateDirective,
                                "Duplicate dts-v1 version header",
                            ))
                        } else if ctx.first_non_include {
                            ctx.add_diagnostic(Diagnostic::from_token(
                                tok.clone(),
                                ErrorCode::MisplacedDtsHeader,
                                "dts-v1 header must be placed on top of the file",
                            ))
                        }
                        ctx.dts_header_seen = true;
                    }
                    AnyDirective::Memreserve(_) => ctx.first_non_include = true,
                    AnyDirective::Include(include) => self.analyze_include(&mut ctx, file, include),
                    AnyDirective::Plugin(_) => {
                        ctx.first_non_include = true;
                        ctx.is_plugin = true
                    }
                    AnyDirective::OmitIfNoRef(..) => ctx.first_non_include = true,
                    AnyDirective::DeletedNode(_, reference) => {
                        self.resolve_reference(&mut ctx, reference);
                    }
                },
                Primary::Root(root_node) => {
                    self.analyze_node(&mut ctx, root_node.clone(), Path::empty());
                    ctx.first_non_include = true
                }
                Primary::ReferencedNode(referenced_node) => {
                    self.analyze_referenced_node(&mut ctx, referenced_node.clone());
                    ctx.first_non_include = true
                }
                Primary::CStyleInclude(_) => {}
            }
        }
        if !ctx.dts_header_seen && ctx.file_type == FileType::DtSource {
            ctx.add_diagnostic(Diagnostic::new(
                Position::zero().as_span(),
                file.source(),
                ErrorCode::NonDtsV1,
                "Files without the '/dts-v1/' Header are not supported",
            ))
        }
        self.resolve_references(&mut ctx);
        ctx.into_result()
    }

    fn analyze_include(&mut self, ctx: &mut FileContext<'_>, parent: &DtsFile, include: &Include) {
        let path = match self.loader.load(&parent.source, &include.file_name()) {
            Ok(path) => path,
            Err(err) => {
                ctx.add_diagnostic(Diagnostic::io_error(include.span(), include.source(), err));
                return;
            }
        };
        if let Err(err) = self
            .import_guard
            .add(path.clone(), &[parent.source.clone().to_path_buf()])
        {
            ctx.add_diagnostic(Diagnostic::cyclic_dependency_error(
                include.span(),
                include.source(),
                err,
            ));
            return;
        }
        let Some(proj_file) = ctx.project.get_file(&path) else {
            return;
        };
        if proj_file.has_errors(&ctx.project.severities) {
            ctx.add_diagnostic(Diagnostic::new(
                include.span(),
                include.source(),
                ErrorCode::ErrorsInInclude,
                "Included file contains errors",
            ));
        }
    }

    fn unresolved_reference_error(
        &self,
        ctx: &mut FileContext<'_>,
        span: Span,
        source: Arc<StdPath>,
    ) {
        // Do not emit unresolved reference errors when we are not a plugin.
        // This will emit false positives as references can only be resolved with the full
        // device-tree information.
        if ctx.file_type == FileType::DtSource && !ctx.is_plugin {
            ctx.add_diagnostic(Diagnostic::new(
                span,
                source,
                ErrorCode::UnresolvedReference,
                "Reference cannot be resolved",
            ));
        }
    }

    fn resolve_reference(
        &mut self,
        ctx: &mut FileContext<'_>,
        reference: &WithToken<Reference>,
    ) -> Path {
        match reference.item() {
            Reference::Label(label) => {
                let resolved =
                    self.context.flat_nodes.iter().find(|(_, value)| {
                        value.label.as_ref().map(|node| node.item()) == Some(label)
                    });
                match resolved {
                    None => {
                        self.unresolved_reference_error(ctx, reference.span(), reference.source());
                        Path::empty()
                    }
                    Some((path, _)) => path.clone(),
                }
            }
            Reference::Path(path) => {
                if !self.context.flat_nodes.contains_key(path) {
                    self.unresolved_reference_error(ctx, reference.span(), reference.source());
                };
                path.clone()
            }
            Reference::PropertyPath(path) => {
                self.resolve_property_path(ctx, reference.span(), reference.source(), path);
                path.node_path().clone()
            }
        }
    }

    pub fn analyze_referenced_node(
        &mut self,
        ctx: &mut FileContext<'_>,
        node: Arc<ReferencedNode>,
    ) {
        if let Some(label) = &node.label {
            self.context
                .labels
                .insert(label.item().clone(), Labeled::ReferencedNode(node.clone()));
        }

        let path = if ctx.file_type == FileType::DtSource {
            self.resolve_reference(ctx, &node.reference)
        } else {
            // This is an include; simply assume the 'root' path
            Path::empty()
        };
        self.analyze_node_payload(ctx, &node.payload, path);
    }

    pub fn resolve_references(&self, ctx: &mut FileContext<'_>) {
        for reference in ctx.unresolved_references.clone() {
            let span = reference.span();
            let source = reference.source();
            match &reference.item() {
                Reference::Label(label) => match self.context.labels.get(label) {
                    Some(_) => {}
                    None => {
                        self.unresolved_reference_error(ctx, span, source);
                    }
                },
                Reference::Path(path) => {
                    if !self.context.flat_nodes.contains_key(path) {
                        self.unresolved_reference_error(ctx, span, source);
                    }
                }
                Reference::PropertyPath(path) => {
                    self.resolve_property_path(ctx, span, source, path);
                }
            }
        }
    }

    fn resolve_property_path(
        &self,
        ctx: &mut FileContext<'_>,
        span: Span,
        source: Arc<StdPath>,
        path: &PropertyPath,
    ) {
        if let Some(node) = self.context.flat_nodes.get(path.node_path()) {
            let payload = &node.payload;
            let property_name = path.property_name();
            if !payload.items.iter().any(|item| {
                matches!(item, NodeItem::Property(property)
                    if property.name.item() == property_name)
            }) {
                self.unresolved_reference_error(ctx, span, source);
            }
        } else {
            self.unresolved_reference_error(ctx, span, source);
        }
    }

    pub fn analyze_node(&mut self, ctx: &mut FileContext<'_>, node: Arc<Node>, path: Path) {
        if let Some(label) = &node.label {
            self.context
                .labels
                .insert(label.item().clone(), Labeled::Node(node.clone()));
        }
        self.context.flat_nodes.insert(path.clone(), node.clone());
        self.analyze_node_payload(ctx, &node.payload, path)
    }

    fn analyze_node_payload(
        &mut self,
        ctx: &mut FileContext<'_>,
        payload: &NodePayload,
        path: Path,
    ) {
        for item in &payload.items {
            match item {
                NodeItem::Property(property) => self.analyze_property(ctx, property.clone()),
                NodeItem::Node(node) => {
                    self.analyze_node(ctx, node.clone(), path.with_child(node.name.item().clone()))
                }
                NodeItem::DeletedNode(..) => {}
                NodeItem::DeletedProperty(..) => {}
            }
        }
    }

    fn check_is_string_list(&mut self, ctx: &mut FileContext<'_>, property: &Property) {
        for value in &property.values {
            if !matches!(value, PropertyValue::String(_)) {
                ctx.add_diagnostic(Diagnostic::new(
                    value.span(),
                    value.source(),
                    ErrorCode::NonStringInCompatible,
                    "compatible property should only contain strings",
                ))
            }
        }
    }

    fn check_is_single_string(&mut self, ctx: &mut FileContext<'_>, property: &Property) {
        if property.values.len() == 1 {
            if let PropertyValue::String(_) = &property.values[0] {
                return;
            }
        }
        ctx.add_diagnostic(Diagnostic::new(
            property.span(),
            property.source(),
            ErrorCode::ExpectedString,
            "property should only contain a single string",
        ))
    }

    fn check_is_single_u32(&mut self, ctx: &mut FileContext<'_>, property: &Property) {
        if property.values.len() == 1 {
            if let PropertyValue::Cells(_, cells, _) = &property.values[0] {
                if cells.len() == 1 {
                    if let Cell::Number(_) = cells[0] {
                        return;
                    }
                }
            }
        }
        ctx.add_diagnostic(Diagnostic::new(
            property.span(),
            property.source(),
            ErrorCode::ExpectedU32,
            "property should only contain a single number",
        ))
    }

    pub fn analyze_property(&mut self, ctx: &mut FileContext<'_>, property: Arc<Property>) {
        if let Some(label) = &property.label {
            self.context
                .labels
                .insert(label.item().clone(), Labeled::Property(property.clone()));
        }
        for value in &property.values {
            self.analyze_property_value(ctx, value)
        }

        match property.name.as_str() {
            "compatible" => self.check_is_string_list(ctx, &property),
            "model" => self.check_is_single_string(ctx, &property),
            "phandle" => self.check_is_single_u32(ctx, &property),
            _ => {}
        }
    }

    pub fn analyze_property_value(&mut self, ctx: &mut FileContext<'_>, value: &PropertyValue) {
        match value {
            PropertyValue::String(_) => {}
            PropertyValue::ByteStrings(..) => {}
            PropertyValue::Cells(_, cells, _) => {
                for cell in cells {
                    self.analyze_cell(ctx, cell)
                }
            }
            PropertyValue::Reference(reference) => self.analyze_reference(ctx, reference),
            PropertyValue::Incbin(_, include, _) => self.analyze_incbin(ctx, include),
        }
    }

    pub fn analyze_cell(&mut self, ctx: &mut FileContext<'_>, value: &Cell) {
        match value {
            Cell::Number(_) => {}
            Cell::Reference(reference) => self.analyze_reference(ctx, reference),
            Cell::Expression(_) => {}
        }
    }

    pub fn analyze_reference(
        &mut self,
        ctx: &mut FileContext<'_>,
        reference: &WithToken<Reference>,
    ) {
        ctx.unresolved_references.push(reference.clone())
    }

    pub fn analyze_incbin(&mut self, ctx: &mut FileContext<'_>, include: &Include) {
        if let Err(err) = self.loader.load(&ctx.source, &include.file_name()) {
            ctx.add_diagnostic(Diagnostic::io_error(include.span(), include.source(), err));
        }
    }
}

#[cfg(test)]
mod test {
    use crate::dts::ast::Path;
    use crate::dts::data::{HasSource, HasSpan, Position};
    use crate::dts::error_codes::ErrorCode;
    use crate::dts::test::Code;
    use crate::dts::{Diagnostic, ParserConfig};
    use assert_unordered::assert_eq_unordered;

    #[test]
    pub fn test_duplicate_v1_header() {
        let code = Code::with_config(
            "\
/dts-v1/;

/{
};

/dts-v1/;
",
            ParserConfig {
                unlimited_property_length: false,
            },
        );
        let (diagnostics, _) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![Diagnostic::new(
                Position::new(5, 0).char_to(8),
                code.source(),
                ErrorCode::DuplicateDirective,
                "Duplicate dts-v1 version header"
            ),]
        )
    }

    #[test]
    pub fn test_misplaced_header() {
        let code = Code::with_config(
            "\
/{
};

/memreserve/ 0x10000000 0x4000;

/dts-v1/;
",
            ParserConfig {
                unlimited_property_length: false,
            },
        );
        let (diagnostics, _) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![Diagnostic::new(
                Position::new(5, 0).char_to(8),
                code.source(),
                ErrorCode::MisplacedDtsHeader,
                "dts-v1 header must be placed on top of the file"
            ),]
        )
    }

    #[test]
    pub fn test_illegal_char_in_label() {
        let code = Code::with_config(
            "\
/dts-v1/;

/{
    my_l?abel: some_node {};
    my_label_that_has_more_than_31_characters: other_node {};
    some_other_node {
        another_ill#gal_label: sub_node {};
    };
    illegal_node_name#s {};
};",
            ParserConfig {
                unlimited_property_length: false,
            },
        );
        let (diagnostics, _) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![
                Diagnostic::new(
                    Position::new(8, 21).as_char_span(),
                    code.source(),
                    ErrorCode::IllegalChar,
                    "Illegal char '#' in node name"
                ),
                Diagnostic::new(
                    Position::new(3, 8).as_char_span(),
                    code.source(),
                    ErrorCode::IllegalChar,
                    "Illegal char '?' in label"
                ),
                Diagnostic::new(
                    Position::new(4, 4).char_to(46),
                    code.source(),
                    ErrorCode::NameTooLong,
                    "label should only have 31 characters but has 41 characters"
                ),
                Diagnostic::new(
                    Position::new(6, 19).as_char_span(),
                    code.source(),
                    ErrorCode::IllegalChar,
                    "Illegal char '#' in label"
                ),
            ]
        )
    }

    #[test]
    pub fn test_accept_long_properties() {
        let code = Code::with_config(
            "\
/dts-v1/;

/{
    my_l?abel: some_node {};
    my_label_that_has_more_than_31_characters: other_node {};
    some_other_node {
        another_ill#gal_label: sub_node {};
    };
    illegal_node_name#s {};
};",
            ParserConfig {
                unlimited_property_length: true,
            },
        );
        let (diagnostics, _) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![
                Diagnostic::new(
                    Position::new(8, 21).as_char_span(),
                    code.source(),
                    ErrorCode::IllegalChar,
                    "Illegal char '#' in node name"
                ),
                Diagnostic::new(
                    Position::new(3, 8).as_char_span(),
                    code.source(),
                    ErrorCode::IllegalChar,
                    "Illegal char '?' in label"
                ),
                Diagnostic::new(
                    Position::new(6, 19).as_char_span(),
                    code.source(),
                    ErrorCode::IllegalChar,
                    "Illegal char '#' in label"
                ),
            ]
        )
    }

    #[test]
    pub fn test_resolve_node_names() {
        let code = Code::new(
            "\
/dts-v1/;

/{
    node1: some_node {
        ref-to-node2 = &node2;
        ref-to-node3 = <&node3>;
    };
    node2: some_other_node {
        ref-to-node1 = &node1;
        ref-to-node1-path = &{/some_node};
        ref-to-node4-path = &{/some_other_node/some_node};
        ref-to-node3-path = &{/node3};
        node4: some_node {
            self-reference = &node4;
        };
    };
};",
        );
        let (diagnostics, context) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![
                Diagnostic::new(
                    code.s1("&node3").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
                Diagnostic::new(
                    code.s1("&{/node3}").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
            ]
        );
        assert_eq!(
            context
                .get_node_by_label("node1")
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("some_node").span()
        );
        assert_eq!(
            context
                .get_node_by_label("node2")
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("some_other_node").span(),
        );
        assert_eq!(
            context
                .get_node_by_label("node4")
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("node4: some_node").s1("some_node").span()
        );
        assert!(context.get_node_by_label("node3").is_none())
    }

    #[test]
    pub fn test_resolve_property_values() {
        let code = Code::new(
            "\
/dts-v1/;

/{
    node1: some_node {
        v0 = <0x00>;
        ref-to-node2 = ${/some_other_node/v1};
        bad-ref-to-node2 = ${/some_other_node/v1bad};
        ref-to-node3 = <${/node3/bad1}>;
    };
    node2: some_other_node {
        v1 = <0x10>;
        ref-to-node1-path = ${/some_node/v0};
        ref-to-node4-path = ${/some_other_node/some_node/v4};
        bad-ref-to-node4-path = ${/some_other_node/some_node/v4bad};
        ref-to-node3-path = ${/node3/bad2};
        node4: some_node {
            self-reference = &node4;
            v4 = <0x20>;
        };
    };
};",
        );
        let (diagnostics, context) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![
                Diagnostic::new(
                    code.s1("${/node3/bad1}").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
                Diagnostic::new(
                    code.s1("${/some_other_node/v1bad}").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
                Diagnostic::new(
                    code.s1("${/node3/bad2}").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
                Diagnostic::new(
                    code.s1("${/some_other_node/some_node/v4bad}").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
            ]
        );
        assert_eq!(
            context
                .get_node_by_label("node1")
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("some_node").span()
        );
        assert_eq!(
            context
                .get_node_by_label("node2")
                .expect("Reference should be set")
                .name
                .span(),
            code.s("some_other_node", 3).span(),
        );
        assert_eq!(
            context
                .get_node_by_label("node4")
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("node4: some_node").s1("some_node").span()
        );
        assert!(context.get_node_by_label("node3").is_none())
    }

    #[test]
    pub fn test_resolve_node_paths() {
        let code = Code::new(
            "\
/dts-v1/;

/{
    node1: some_node {
        ref-to-node2 = &node2;
    };
    node2: some_other_node {
        ref-to-node1 = &node1;
        node4: some_node {
            self-reference = &node4;
        };
    };
};",
        );
        let (diag, context) = code.get_analyzed_file();
        assert!(diag.is_empty());
        assert_eq!(
            context
                .get_node_by_path(&Path::new(vec!["some_node".into()]))
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("some_node").span(),
        );
        assert_eq!(
            context
                .get_node_by_path(&Path::new(vec!["some_other_node".into()]))
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("some_other_node").span(),
        );
        assert_eq!(
            context
                .get_node_by_path(&Path::new(vec![
                    "some_other_node".into(),
                    "some_node".into(),
                ]))
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("node4: some_node").s1("some_node").span()
        );
        assert!(context.get_node_by_label("node3").is_none())
    }

    #[test]
    pub fn test_does_not_accept_non_dtsv1_sources() {
        let code = Code::new("/ {};");
        let (diagnostics, _) = code.get_analyzed_file();
        assert_eq!(
            diagnostics,
            vec![Diagnostic::new(
                Position::zero().as_span(),
                code.source(),
                ErrorCode::NonDtsV1,
                "Files without the '/dts-v1/' Header are not supported"
            )]
        )
    }

    #[test]
    pub fn referenced_node_in_same_file() {
        let code = Code::new(
            "\
/dts-v1/;

/ {
    some_node: node {};
};

&some_node {};

&some_other_node {};

&{/node} {};

&{/some_other_node} {};

",
        );
        let (diagnostics, _) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![
                Diagnostic::new(
                    code.s1("&some_other_node").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
                Diagnostic::new(
                    code.s1("&{/some_other_node}").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                )
            ]
        )
    }

    #[test]
    pub fn schema_diagnostics() {
        let code = Code::new(
            r#"
/dts-v1/;

/ {
    compatible = <0x0>;
    model = "foo", "bar";
    phandle = "wat";
};
"#,
        );
        let (diagnostics, _) = code.get_analyzed_file();
        assert_eq_unordered!(
            diagnostics,
            vec![
                Diagnostic::new(
                    code.s1("<0x0>").span(),
                    code.source(),
                    ErrorCode::NonStringInCompatible,
                    "compatible property should only contain strings"
                ),
                Diagnostic::new(
                    code.s1("model = \"foo\", \"bar\";").span(),
                    code.source(),
                    ErrorCode::ExpectedString,
                    "property should only contain a single string"
                ),
                Diagnostic::new(
                    code.s1("phandle = \"wat\";").span(),
                    code.source(),
                    ErrorCode::ExpectedU32,
                    "property should only contain a single number"
                )
            ]
        )
    }
}
