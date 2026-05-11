use itertools::Itertools;

use crate::dts::ast::{
    ABSOLUTE_ROOT, AbsolutePath, AnyDirective, Cell, DtsFile, Include, LabelRelativePath, Node,
    NodeItem, NodeName, NodePayload, Path, Primary, Property, PropertyPath, PropertyValue,
    Reference, ReferencedNode, WithToken,
};
use crate::dts::data::{HasSource, HasSpan, Span};
use crate::dts::error_codes::ErrorCode;
use crate::dts::import_guard::ImportGuard;
use crate::dts::loader::IncludeLoaderGuard;
use crate::dts::visitor::ReferenceContext;
use crate::dts::{Diagnostic, FileType, Position, Project};
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::Arc;

fn find_child(payload: &NodePayload, path_elements: &[NodeName]) -> Option<Arc<Node>> {
    if path_elements.is_empty() {
        return None;
    }
    let last = path_elements.len() - 1;
    let mut current = payload;
    for (i, element) in path_elements.iter().enumerate() {
        for item in &current.items {
            if let NodeItem::Node(node) = item {
                if node.name.item() == element {
                    if i == last {
                        return Some(node.clone());
                    } else {
                        current = &node.payload;
                    }
                }
            }
        }
    }
    None
}

fn find_node_child(node: &Arc<Node>, path_elements: &[NodeName]) -> Option<Arc<Node>> {
    if path_elements.is_empty() {
        return Some(node.clone());
    }
    find_child(&node.payload, path_elements)
}

/// Something that can be labeled.
/// Used when analyzing a device-tree
#[derive(Clone, Debug)]
enum Labeled {
    Node(Arc<Node>, AbsolutePath),
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
    flat_nodes: HashMap<AbsolutePath, Arc<Node>>,
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
            Some(Labeled::Node(node, _)) => Some(node),
            _ => None,
        }
    }

    fn node_from_ctx<'a>(&'a self, ctx: &ReferenceContext<'a>) -> &'a Node {
        match ctx {
            ReferenceContext::Root => self
                .flat_nodes
                .get(&ABSOLUTE_ROOT)
                .map(|v| &**v)
                .expect("root node must exist"),
            ReferenceContext::Node(node) => node,
        }
    }

    pub fn get_node_by_path(&self, path: &Path, ctx: &ReferenceContext<'_>) -> Option<Arc<Node>> {
        match path {
            Path::Absolute(absolute_path) => self.flat_nodes.get(absolute_path).cloned(),
            Path::DotRelative(_) => find_child(&self.node_from_ctx(ctx).payload, path.elements()),
            Path::LabelRelative(label_relative_path) => {
                match self.labels.get(&label_relative_path.label)? {
                    Labeled::Node(node, _) => find_node_child(node, path.elements()),
                    Labeled::Property(_) => None,
                    Labeled::ReferencedNode(referenced_node) => {
                        if !path.elements().is_empty() {
                            let found = find_child(&referenced_node.payload, path.elements());
                            if found.is_some() {
                                return found;
                            }
                        }
                        match self.labels.get(&label_relative_path.label)? {
                            Labeled::Node(node, _) => find_node_child(node, path.elements()),
                            _ => None,
                        }
                    }
                }
            }
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

    fn get_property_from_payload(
        &self,
        property_name: &str,
        payload: &NodePayload,
    ) -> Option<Arc<Property>> {
        for item in &payload.items {
            if let NodeItem::Property(property) = item {
                if property.name.item() == property_name {
                    return Some(property.clone());
                }
            }
        }
        None
    }

    fn get_property_from_property_path(
        &self,
        path: &PropertyPath,
        ctx: &ReferenceContext<'_>,
    ) -> Option<Arc<Property>> {
        let node = self.get_node_by_path(path.node_path(), ctx)?;
        let payload = &node.payload;
        self.get_property_from_payload(path.property_name(), payload)
    }

    pub fn get_position(
        &self,
        reference: &Reference,
        ctx: &ReferenceContext<'_>,
    ) -> Option<(Span, Arc<std::path::Path>)> {
        match reference {
            Reference::Label(label) => match self.labels.get(label) {
                Some(Labeled::Node(node, _)) => Some(self.get_node_position(node)),
                Some(Labeled::ReferencedNode(node)) => self.get_referenced_node_position(node),
                _ => None,
            },
            Reference::Path(path) => self
                .get_node_by_path(path, ctx)
                .map(|node| self.get_node_position(&node)),
            Reference::PropertyPath(path) => {
                let property = self.get_property_from_property_path(path, ctx)?;
                Some((property.name.span(), property.name.source()))
            }
        }
    }

    pub fn get_referred(
        &self,
        reference: &Reference,
        ctx: &ReferenceContext<'_>,
    ) -> Option<String> {
        match reference {
            Reference::Label(label) => match self.labels.get(label) {
                Some(Labeled::Node(node, _)) => {
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
                .get_node_by_path(path, ctx)
                .map(|node| node.name.name.clone()),
            Reference::PropertyPath(path) => {
                let property = self.get_property_from_property_path(path, ctx)?;
                Some(property.values.iter().map(|v| v.to_string()).join(", "))
            }
        }
    }
}

pub struct FileContext<'a> {
    source: Arc<StdPath>,
    project: &'a Project,
    diagnostics: Vec<Diagnostic>,
    references_to_be_checked: Vec<WithToken<Reference>>,
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
            references_to_be_checked: Vec::default(),
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
                        self.get_absolute_path_from_reference_in_root_ctx(&mut ctx, reference);
                    }
                },
                Primary::Root(root_node) => {
                    self.analyze_node(&mut ctx, root_node.clone(), &ABSOLUTE_ROOT);
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

    fn get_valid_absolute_path(&self, absolute_path: &AbsolutePath) -> Option<AbsolutePath> {
        if self.context.flat_nodes.contains_key(absolute_path) {
            Some(absolute_path.clone())
        } else {
            None
        }
    }

    fn get_valid_absolute_path_from_label_relative_path(
        &self,
        label_relative_path: &LabelRelativePath,
    ) -> Option<AbsolutePath> {
        let (node, label_absolute_path) = match self.context.labels.get(&label_relative_path.label)
        {
            Some(Labeled::Node(node, path)) => Some((node, path)),
            _ => None,
        }?;

        let elements = label_relative_path.elements();
        find_node_child(node, elements)
            .is_some()
            .then(|| label_absolute_path.with_children(elements))
    }

    fn get_absolute_path_from_path_in_root_ctx(&self, path: &Path) -> Option<AbsolutePath> {
        match path {
            Path::Absolute(absolute_path) => self.get_valid_absolute_path(absolute_path),
            Path::DotRelative(_dot_relative_path) => {
                // dot-relative-paths do not resolve in a root ctx
                None
            }
            Path::LabelRelative(label_relative_path) => {
                self.get_valid_absolute_path_from_label_relative_path(label_relative_path)
            }
        }
    }

    fn get_absolute_path_from_property_path_in_root_ctx(
        &self,
        ppath: &PropertyPath,
    ) -> Option<AbsolutePath> {
        let absolute_path = self.get_absolute_path_from_path_in_root_ctx(ppath.node_path())?;
        let node = self.context.flat_nodes.get(&absolute_path)?;
        let payload = &node.payload;
        let property_name = ppath.property_name();
        if !payload.items.iter().any(|item| {
            matches!(item, NodeItem::Property(property)
                if property.name.item() == property_name)
        }) {
            return None;
        }

        Some(absolute_path)
    }

    fn get_absolute_path_from_reference_in_root_ctx(
        &mut self,
        ctx: &mut FileContext<'_>,
        reference: &WithToken<Reference>,
    ) -> Option<AbsolutePath> {
        let path = match reference.item() {
            Reference::Label(label) => {
                if let Some(Labeled::Node(_node, absolute_path)) = self.context.labels.get(label) {
                    Some(absolute_path.clone())
                } else {
                    None
                }
            }
            Reference::Path(path) => self.get_absolute_path_from_path_in_root_ctx(path),
            Reference::PropertyPath(ppath) => {
                self.get_absolute_path_from_property_path_in_root_ctx(ppath)
            }
        };
        if path.is_none() {
            self.unresolved_reference_error(ctx, reference.span(), reference.source());
        }
        path
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

        let path = self
            .get_absolute_path_from_reference_in_root_ctx(ctx, &node.reference)
            .unwrap_or(ABSOLUTE_ROOT.clone());

        self.analyze_node_payload(ctx, &node.payload, &path);
    }

    pub fn resolve_references(&self, ctx: &mut FileContext<'_>) {
        for reference in ctx.references_to_be_checked.clone() {
            let span = reference.span();
            let source = reference.source();
            let ok = match &reference.item() {
                Reference::Label(label) => self.context.labels.contains_key(label),
                Reference::Path(path) => {
                    self.get_absolute_path_from_path_in_root_ctx(path).is_some()
                }
                Reference::PropertyPath(ppath) => self
                    .get_absolute_path_from_property_path_in_root_ctx(ppath)
                    .is_some(),
            };
            if !ok {
                self.unresolved_reference_error(ctx, span, source);
            }
        }
    }

    pub fn analyze_node(
        &mut self,
        ctx: &mut FileContext<'_>,
        node: Arc<Node>,
        path: &AbsolutePath,
    ) {
        if let Some(label) = &node.label {
            self.context.labels.insert(
                label.item().clone(),
                Labeled::Node(node.clone(), path.clone()),
            );
        }
        self.context.flat_nodes.insert(path.clone(), node.clone());
        self.analyze_node_payload(ctx, &node.payload, path)
    }

    fn analyze_node_payload(
        &mut self,
        ctx: &mut FileContext<'_>,
        payload: &NodePayload,
        path: &AbsolutePath,
    ) {
        for item in &payload.items {
            match item {
                NodeItem::Property(property) => {
                    self.analyze_property(ctx, property.clone(), payload, path)
                }
                NodeItem::Node(node) => self.analyze_node(
                    ctx,
                    node.clone(),
                    &path.with_child(node.name.item().clone()),
                ),
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

    fn check_is_single_string(
        &mut self,
        ctx: &mut FileContext<'_>,
        property: &Property,
    ) -> Option<String> {
        if property.values.len() == 1 {
            if let PropertyValue::String(s) = &property.values[0] {
                return Some(s.item().clone());
            }
        }
        ctx.add_diagnostic(Diagnostic::new(
            property.span(),
            property.source(),
            ErrorCode::ExpectedString,
            "property should contain a single string",
        ));
        None
    }

    fn check_is_single_u32(
        &mut self,
        ctx: &mut FileContext<'_>,
        property: &Property,
    ) -> Option<u32> {
        if property.values.len() == 1 {
            if let PropertyValue::Cells(_, cells, _) = &property.values[0] {
                if cells.len() == 1 {
                    if let Cell::Number(n, _) = &cells[0] {
                        return Some(*n.item());
                    }
                }
            }
        }
        ctx.add_diagnostic(Diagnostic::new(
            property.span(),
            property.source(),
            ErrorCode::ExpectedU32,
            "property should contain a single number",
        ));
        None
    }

    pub fn analyze_property(
        &mut self,
        ctx: &mut FileContext<'_>,
        property: Arc<Property>,
        in_node: &NodePayload,
        path: &AbsolutePath,
    ) {
        if let Some(label) = &property.label {
            self.context
                .labels
                .insert(label.item().clone(), Labeled::Property(property.clone()));
        }
        for value in &property.values {
            self.analyze_property_value(ctx, value, in_node)
        }

        match property.name.as_str() {
            "compatible" => {
                self.check_is_string_list(ctx, &property);
            }
            "model" => {
                if path == &ABSOLUTE_ROOT {
                    self.check_is_single_string(ctx, &property);
                }
            }
            "phandle" => {
                self.check_is_single_u32(ctx, &property);
            }
            _ => {}
        }
    }

    pub fn analyze_property_value(
        &mut self,
        ctx: &mut FileContext<'_>,
        value: &PropertyValue,
        in_node: &NodePayload,
    ) {
        match value {
            PropertyValue::String(_) => {}
            PropertyValue::ByteStrings(..) => {}
            PropertyValue::Cells(_, cells, _) => {
                for cell in cells {
                    self.analyze_cell(ctx, cell, in_node)
                }
            }
            PropertyValue::Reference(reference) => self.analyze_reference(ctx, reference, in_node),
            PropertyValue::Incbin(_, include, _) => self.analyze_incbin(ctx, include, value.span()),
        }
    }

    pub fn analyze_cell(&mut self, ctx: &mut FileContext<'_>, value: &Cell, in_node: &NodePayload) {
        match value {
            Cell::Number(_, _) => {}
            Cell::Reference(reference) => self.analyze_reference(ctx, reference, in_node),
            Cell::Expression(_) => {}
        }
    }

    pub fn analyze_reference(
        &mut self,
        ctx: &mut FileContext<'_>,
        reference: &WithToken<Reference>,
        in_node: &NodePayload,
    ) {
        // Check dot-relative references in context.
        //
        // We only produce a bool result here for references containing dot-relative paths.
        // For everything else, we will push the list of references to be checked later,
        // at the end of the analysis.
        let ok: Option<bool> = match reference.item() {
            Reference::Path(Path::DotRelative(dot_relative)) => {
                Some(find_child(in_node, dot_relative.elements()).is_some())
            }
            Reference::PropertyPath(property_path) => match property_path.node_path() {
                Path::DotRelative(dot_relative) => {
                    Some(match find_child(in_node, dot_relative.elements()) {
                        Some(child) => {
                            let name = property_path.property_name();
                            self.context
                                .get_property_from_payload(name, &child.payload)
                                .is_some()
                        }
                        None => false,
                    })
                }
                _ => None,
            },
            _ => None,
        };

        if let Some(ok) = ok {
            // Push a diagnostic if we got an error.
            if !ok {
                self.unresolved_reference_error(ctx, reference.span(), reference.source());
            }
        } else {
            // Check all other kinds later.
            ctx.references_to_be_checked.push(reference.clone());
        }
    }

    pub fn analyze_incbin(&mut self, ctx: &mut FileContext<'_>, include: &Include, span: Span) {
        if let Err(err) = self.loader.load(&ctx.source, &include.file_name()) {
            ctx.add_diagnostic(Diagnostic::io_error(span, include.source(), err));
        }
    }
}

#[cfg(test)]
mod test {
    use crate::dts::ast::{Path, PropertyPath};
    use crate::dts::data::{HasSource, HasSpan, Position};
    use crate::dts::error_codes::ErrorCode;
    use crate::dts::test::Code;
    use crate::dts::visitor::ReferenceContext;
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
        dot-ref-to-node4-path = &{./some_node};
        dot-ref-to-bad-path = &{./some_node/missing};
        ref-to-label = &{node1};
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
                Diagnostic::new(
                    code.s1("&{./some_node/missing}").span(),
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
        dot-ref-to-node4-path = ${./some_node/v4};
        dot-ref-to-bad-path = ${./some_node/missing};
        dot-ref-to-bad-sub-path = ${./some_node/sub/v4};
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
                Diagnostic::new(
                    code.s1("${./some_node/missing}").span(),
                    code.source(),
                    ErrorCode::UnresolvedReference,
                    "Reference cannot be resolved"
                ),
                Diagnostic::new(
                    code.s1("${./some_node/sub/v4}").span(),
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
                .get_node_by_path(
                    &Path::new_absolute(vec!["some_node".into()]),
                    &ReferenceContext::Root
                )
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("some_node").span(),
        );
        assert_eq!(
            context
                .get_node_by_path(
                    &Path::new_dot_relative(vec!["some_other_node".into(), "some_node".into()]),
                    &ReferenceContext::Root
                )
                .expect("Reference should be set")
                .name
                .span(),
            code.s("some_node", 2).span(),
        );
        assert_eq!(
            context
                .get_node_by_path(
                    &Path::new_absolute(vec!["some_other_node".into()]),
                    &ReferenceContext::Root
                )
                .expect("Reference should be set")
                .name
                .span(),
            code.s1("some_other_node").span(),
        );
        assert_eq!(
            context
                .get_node_by_path(
                    &Path::new_absolute(vec!["some_other_node".into(), "some_node".into(),]),
                    &ReferenceContext::Root
                )
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
    pub fn labeled_referenced() {
        let code = Code::new(
            "\
/dts-v1/;

/ {
    some_node: node {
            child-2 {
            };
    };
};

labeled_referenced: &some_node {
    child-1 {
    };
};
",
        );
        let (diagnostics, context) = code.get_analyzed_file();
        assert_eq_unordered!(diagnostics, vec![]);

        let node = context
            .get_node_by_path(
                &Path::new_label_relative("labeled_referenced".into(), vec!["child-1".into()]),
                &ReferenceContext::Root,
            )
            .unwrap();
        assert_eq!(
            context.get_node_position(&node).0,
            code.s1("child-1").span()
        );
    }

    #[test]
    pub fn test_get_position() {
        let code = Code::new(
            "\
/dts-v1/;

/ {
    some_label: node {
        relative = ${./another/prop};
        second_label: another {
            prop = <0>;
        };
    };
};

labeled_referenced: &some_label {
    more = <0>;
};
",
        );
        let (diagnostics, analysis) = code.get_analyzed_file();
        assert_eq_unordered!(diagnostics, vec![]);
        assert_eq!(
            analysis.get_position(
                &crate::dts::ast::Reference::Label("labeled_referenced".to_string()),
                &ReferenceContext::Root
            ),
            Some((code.s1("labeled_referenced:").span(), code.source()))
        );
        assert_eq!(
            analysis.get_position(
                &crate::dts::ast::Reference::Label("some_label".to_string()),
                &ReferenceContext::Root
            ),
            Some((code.s1("node").span(), code.source()))
        );
        assert_eq!(
            analysis.get_position(
                &crate::dts::ast::Reference::Path(Path::new_absolute(vec![
                    "node".into(),
                    "another".into()
                ])),
                &ReferenceContext::Root
            ),
            Some((code.s("another", 2).span(), code.source()))
        );
        assert_eq!(
            analysis.get_position(
                &crate::dts::ast::Reference::Path(Path::new_label_relative(
                    "some_label".into(),
                    vec!["another".into()]
                )),
                &ReferenceContext::Root
            ),
            Some((code.s("another", 2).span(), code.source()))
        );
        assert_eq!(
            analysis.get_position(
                &crate::dts::ast::Reference::PropertyPath(PropertyPath::new(
                    Path::new_label_relative("some_label".into(), vec!["another".into()]),
                    "prop".into(),
                )),
                &ReferenceContext::Root
            ),
            Some((code.s("prop", 2).span(), code.source()))
        );
    }

    #[test]
    pub fn test_get_referred() {
        let code = Code::new(
            "\
/dts-v1/;

/ {
    some_label: node@00001111 {
        relative = ${./another/prop};
        second_label: another {
            prop = <0>;
        };
    };
};

labeled_referenced: &some_label {
    more = <0>;
};
",
        );
        let (diagnostics, analysis) = code.get_analyzed_file();
        assert_eq_unordered!(diagnostics, vec![]);
        assert_eq!(
            analysis.get_referred(
                &crate::dts::ast::Reference::Label("labeled_referenced".to_string()),
                &ReferenceContext::Root
            ),
            // TODO: check: is this the result we want?
            Some("labeled_referenced".into())
        );
        assert_eq!(
            analysis.get_referred(
                &crate::dts::ast::Reference::Label("some_label".to_string()),
                &ReferenceContext::Root
            ),
            Some("node@00001111".into())
        );
        assert_eq!(
            analysis.get_referred(
                &crate::dts::ast::Reference::Path(Path::new_absolute(vec![
                    "node@00001111".into(),
                    "another".into()
                ])),
                &ReferenceContext::Root
            ),
            Some("another".into())
        );
        // A node without its unit address does not resolve in a path:
        assert_eq!(
            analysis.get_referred(
                &crate::dts::ast::Reference::Path(Path::new_absolute(vec![
                    "node".into(),
                    "another".into()
                ])),
                &ReferenceContext::Root
            ),
            None
        );
        assert_eq!(
            analysis.get_referred(
                &crate::dts::ast::Reference::Path(Path::new_label_relative(
                    "some_label".into(),
                    vec!["another".into()]
                )),
                &ReferenceContext::Root
            ),
            Some("another".into())
        );
        assert_eq!(
            analysis.get_referred(
                &crate::dts::ast::Reference::PropertyPath(PropertyPath::new(
                    Path::new_label_relative("some_label".into(), vec!["another".into()]),
                    "prop".into(),
                )),
                &ReferenceContext::Root
            ),
            Some("<0>".into())
        );
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

    good {
        compatible = "aaa,bbb", "ccc,ddd";
        model = "bla";
        labeled: phandle = <also_labeled: 1>;
    };

    ignore {
        model = "foo", "bar";
    };
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
                    "property should contain a single string"
                ),
                Diagnostic::new(
                    code.s1("phandle = \"wat\";").span(),
                    code.source(),
                    ErrorCode::ExpectedU32,
                    "property should contain a single number"
                )
            ]
        )
    }
}
