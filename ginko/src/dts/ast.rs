use crate::dts::data::HasSource;
use crate::dts::tokens::Token;
use crate::dts::{HasSpan, Span};
use itertools::Itertools;
use std::fmt::{Display, Formatter, LowerHex};
use std::ops::Deref;
use std::path::Path as StdPath;
use std::sync::Arc;

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct WithToken<T> {
    item: T,
    token: Token,
}

impl<T> HasSpan for WithToken<T> {
    fn span(&self) -> Span {
        self.token.span
    }
}

impl<T> HasSource for WithToken<T> {
    fn source(&self) -> Arc<StdPath> {
        self.token.source()
    }
}

impl<T> WithToken<T> {
    pub fn new(item: T, token: Token) -> WithToken<T> {
        WithToken { item, token }
    }

    pub fn item(&self) -> &T {
        &self.item
    }
}

impl<T> Deref for WithToken<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.item
    }
}

impl<T> Display for WithToken<T>
where
    T: Display,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.item())
    }
}

impl<T> LowerHex for WithToken<T>
where
    T: LowerHex,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:x}", self.item())
    }
}

// LRM 2.2.1 – Node Names
#[derive(Eq, PartialEq, Debug, Hash, Clone)]
pub struct NodeName {
    pub name: String,
    pub unit_address: Option<String>,
}

impl From<String> for NodeName {
    fn from(value: String) -> Self {
        if let Some((prefix, suffix)) = value.split_once('@') {
            NodeName::with_address(prefix, suffix)
        } else {
            NodeName::simple(value)
        }
    }
}

impl From<&str> for NodeName {
    fn from(value: &str) -> Self {
        if let Some((prefix, suffix)) = value.split_once('@') {
            NodeName::with_address(prefix, suffix)
        } else {
            NodeName::simple(value)
        }
    }
}

impl From<WithToken<String>> for WithToken<NodeName> {
    fn from(value: WithToken<String>) -> Self {
        WithToken::new(NodeName::from(value.item), value.token)
    }
}

impl NodeName {
    pub fn simple(name: impl Into<String>) -> NodeName {
        NodeName {
            name: name.into(),
            unit_address: None,
        }
    }

    pub fn with_address(name: impl Into<String>, address: impl Into<String>) -> NodeName {
        NodeName {
            name: name.into(),
            unit_address: Some(address.into()),
        }
    }
}

impl Display for NodeName {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name)?;
        if let Some(unit_address) = &self.unit_address {
            write!(f, "@{}", unit_address)?;
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq, Debug, Hash, Clone)]
pub struct AbsolutePath {
    elements: Vec<NodeName>,
}

pub static ABSOLUTE_ROOT: AbsolutePath = AbsolutePath { elements: vec![] };

impl AbsolutePath {
    pub fn with_child(&self, child: NodeName) -> Self {
        self.with_children(&[child])
    }

    pub fn with_children(&self, children: &[NodeName]) -> Self {
        let mut clone = self.clone();
        let elements = &mut clone.elements;
        elements.extend_from_slice(children);
        clone
    }
}

impl Display for AbsolutePath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.elements.is_empty() {
            write!(f, "/")
        } else {
            for element in &self.elements {
                write!(f, "/{}", element)?;
            }
            Ok(())
        }
    }
}

#[derive(Eq, PartialEq, Debug, Hash, Clone)]
pub struct DotRelativePath {
    elements: Vec<NodeName>,
}

impl DotRelativePath {
    pub fn elements(&self) -> &[NodeName] {
        &self.elements
    }
}

impl Display for DotRelativePath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, ".")?;
        if self.elements.is_empty() {
            write!(f, "/")
        } else {
            for element in &self.elements {
                write!(f, "/{}", element)?;
            }
            Ok(())
        }
    }
}

#[derive(Eq, PartialEq, Debug, Hash, Clone)]
pub struct LabelRelativePath {
    pub label: String,
    elements: Vec<NodeName>,
}

impl LabelRelativePath {
    pub fn elements(&self) -> &[NodeName] {
        &self.elements
    }
}

impl Display for LabelRelativePath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label)?;
        if !self.elements.is_empty() {
            for element in &self.elements {
                write!(f, "/{}", element)?;
            }
        }
        Ok(())
    }
}

// LRM 2.2.3 – Paths
#[derive(Eq, PartialEq, Debug, Hash, Clone)]
pub enum Path {
    Absolute(AbsolutePath),
    DotRelative(DotRelativePath),
    LabelRelative(LabelRelativePath),
}

impl Display for Path {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self {
            Path::Absolute(p) => p.fmt(f),
            Path::DotRelative(p) => p.fmt(f),
            Path::LabelRelative(p) => p.fmt(f),
        }
    }
}

impl Path {
    pub fn new_absolute(elements: Vec<NodeName>) -> Path {
        Path::Absolute(AbsolutePath { elements })
    }

    pub fn new_dot_relative(elements: Vec<NodeName>) -> Path {
        Path::DotRelative(DotRelativePath { elements })
    }

    pub fn new_label_relative(label: String, elements: Vec<NodeName>) -> Path {
        Path::LabelRelative(LabelRelativePath { label, elements })
    }

    pub fn empty() -> Path {
        Self::new_absolute(vec![])
    }

    pub fn elements(&self) -> &Vec<NodeName> {
        match &self {
            Path::Absolute(p) => &p.elements,
            Path::DotRelative(p) => &p.elements,
            Path::LabelRelative(p) => &p.elements,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &NodeName> {
        self.elements().iter()
    }
}

fn split_path(path: &str) -> Vec<NodeName> {
    path.split('/')
        .filter(|component| !component.is_empty())
        .map(NodeName::from)
        .collect_vec()
}

impl<S> From<S> for Path
where
    S: AsRef<str>,
{
    fn from(value: S) -> Self {
        let value = value.as_ref();
        if value.starts_with('/') {
            Path::Absolute(AbsolutePath {
                elements: split_path(value),
            })
        } else if let Some(rest) = value.strip_prefix("./") {
            Path::DotRelative(DotRelativePath {
                elements: split_path(rest),
            })
        } else if let Some((label, rest)) = value.split_once('/') {
            Path::LabelRelative(LabelRelativePath {
                label: label.to_string(),
                elements: split_path(rest),
            })
        } else {
            Path::LabelRelative(LabelRelativePath {
                label: value.to_string(),
                elements: vec![],
            })
        }
    }
}

// osdyne extension: ${/path/to/node/value}
#[derive(Eq, PartialEq, Debug, Hash, Clone)]

pub struct PropertyPath {
    node_path: Path,
    property_name: String,
}

impl PropertyPath {
    pub fn new(node_path: Path, property_name: String) -> PropertyPath {
        PropertyPath {
            node_path,
            property_name,
        }
    }

    pub fn node_path(&self) -> &Path {
        &self.node_path
    }

    pub fn property_name(&self) -> &str {
        &self.property_name
    }
}

impl From<&str> for PropertyPath {
    fn from(value: &str) -> Self {
        if let Some((node_path, property_name)) = value.rsplit_once('/') {
            PropertyPath::new(Path::from(node_path), property_name.to_string())
        } else {
            PropertyPath::new(Path::empty(), value.to_string())
        }
    }
}

impl Display for PropertyPath {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.node_path() {
            Path::Absolute(_) => {
                if self.node_path().elements().is_empty() {
                    write!(f, "/{}", self.property_name)
                } else {
                    write!(f, "{}/{}", self.node_path, self.property_name)
                }
            }
            Path::DotRelative(_) => {
                if self.node_path().elements().is_empty() {
                    write!(f, "./{}", self.property_name)
                } else {
                    write!(f, "{}/{}", self.node_path, self.property_name)
                }
            }
            Path::LabelRelative(_) => {
                write!(f, "{}/{}", self.node_path, self.property_name)
            }
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub enum Reference {
    Label(String),
    Path(Path),
    PropertyPath(PropertyPath),
}

impl Display for Reference {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Reference::Label(label) => write!(f, "&{label}"),
            Reference::Path(path) => write!(f, "&{{{path}}}"),
            Reference::PropertyPath(path) => write!(f, "${{{path}}}"),
        }
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub enum NumberRepr {
    Hexadecimal,
    Octal,
    Decimal,
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub enum Cell {
    Number(WithToken<u32>, NumberRepr),
    Reference(WithToken<Reference>),
    Expression(WithToken<String>),
}

impl Cell {
    pub fn decimal(token: WithToken<u32>) -> Cell {
        Cell::Number(token, NumberRepr::Decimal)
    }
    pub fn hexadecimal(token: WithToken<u32>) -> Cell {
        Cell::Number(token, NumberRepr::Hexadecimal)
    }
}

impl Display for Cell {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Cell::Number(num, NumberRepr::Hexadecimal) => write!(f, "0x{num:x}"),
            Cell::Number(num, NumberRepr::Octal) => write!(f, "0{:o}", *num.item() as i32),
            Cell::Number(num, NumberRepr::Decimal) => write!(f, "{num}"),
            Cell::Reference(reference) => write!(f, "{reference}"),
            Cell::Expression(exp) => write!(f, "{exp}"),
        }
    }
}

// LRM 2.2.4 Property Values
#[derive(Eq, PartialEq, Debug)]
pub enum PropertyValue {
    String(WithToken<String>),
    Cells(Token, Vec<Cell>, Token),
    Reference(WithToken<Reference>),
    ByteStrings(Token, Vec<WithToken<Vec<u8>>>, Token),
    Incbin(Token, Include, Token),
}

impl HasSpan for PropertyValue {
    fn span(&self) -> Span {
        match self {
            PropertyValue::String(str) => str.span(),
            PropertyValue::Cells(start, _, end) => start.start().to(end.end()),
            PropertyValue::Reference(reference) => reference.span(),
            PropertyValue::ByteStrings(start, _, end) => start.start().to(end.end()),
            PropertyValue::Incbin(_, include, end) => include.include_token.start().to(end.end()),
        }
    }
}

impl HasSource for PropertyValue {
    fn source(&self) -> Arc<StdPath> {
        match self {
            PropertyValue::String(str) => str.token.source(),
            PropertyValue::Cells(start, ..) => start.source.clone(),
            PropertyValue::Reference(reference) => reference.token.source.clone(),
            PropertyValue::ByteStrings(start, ..) => start.source.clone(),
            PropertyValue::Incbin(start, ..) => start.source.clone(),
        }
    }
}

impl Display for PropertyValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match &self {
            PropertyValue::String(string) => {
                write!(f, "\"{string}\"")
            }
            PropertyValue::Cells(_, numbers, _) => {
                write!(f, "<")?;
                for (i, num) in numbers.iter().enumerate() {
                    write!(f, "{num}")?;
                    if i != numbers.len() - 1 {
                        write!(f, " ")?;
                    }
                }
                write!(f, ">")
            }
            PropertyValue::Reference(reference) => write!(f, "{reference}",),
            PropertyValue::ByteStrings(_, strings, _) => {
                write!(f, "[")?;
                for (i, numbers) in strings.iter().enumerate() {
                    for num in &numbers.item {
                        write!(f, "{num:2x}")?;
                    }
                    if i != strings.len() - 1 {
                        write!(f, " ")?;
                    }
                }
                write!(f, "]")
            }
            PropertyValue::Incbin(_, incbin_path, _) => {
                write!(f, "/incbin/(\"{incbin_path}\")")
            }
        }
    }
}

// LRM 2.2.4 Property Values
#[derive(Eq, PartialEq, Debug)]
pub struct Property {
    pub label: Option<WithToken<String>>,
    pub name: WithToken<String>,
    pub values: Vec<PropertyValue>,
    pub end: Token,
}

impl HasSource for Property {
    fn source(&self) -> Arc<StdPath> {
        self.end.source()
    }
}

impl HasSpan for Property {
    fn span(&self) -> Span {
        self.label
            .as_ref()
            .map(|label| label.token.span())
            .unwrap_or(self.name.span())
            .start()
            .to(self.end.end())
    }
}

impl Property {
    pub fn empty(
        name: WithToken<String>,
        label: Option<WithToken<String>>,
        end: Token,
    ) -> Property {
        Property {
            label,
            name,
            values: vec![],
            end,
        }
    }
}

impl Display for Property {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.values.is_empty() {
            writeln!(f, "{};", self.name)
        } else {
            write!(f, "{} = ", self.name)?;
            for (i, value) in self.values.iter().enumerate() {
                write!(f, "{value}")?;
                if i != self.values.len() - 1 {
                    write!(f, ", ")?;
                }
            }
            writeln!(f, ";")
        }
    }
}

#[derive(Eq, PartialEq, Debug)]
pub struct Node {
    pub label: Option<WithToken<String>>,
    pub name: WithToken<NodeName>,
    pub payload: NodePayload,
    pub omit_if_no_ref: Option<Token>,
    pub span: Span,
}

#[derive(Eq, PartialEq, Debug)]
pub enum NodeItem {
    Property(Arc<Property>),
    Node(Arc<Node>),
    DeletedNode(Token, WithToken<NodeName>),
    DeletedProperty(Token, WithToken<String>),
}

impl Display for NodeItem {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeItem::Property(property) => write!(f, "{}", property),
            NodeItem::Node(node) => write!(f, "{}", node),
            NodeItem::DeletedNode(_, node_name) => write!(f, "/delete-node/ {}", node_name),
            NodeItem::DeletedProperty(_, property_name) => {
                write!(f, "/delete-property/ {}", property_name)
            }
        }
    }
}

#[derive(Eq, PartialEq, Debug)]
pub struct NodePayload {
    pub items: Vec<NodeItem>,
    pub end: Token,
}

impl Display for NodePayload {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "{{")?;
        for item in &self.items {
            writeln!(f, "    {item}")?;
        }
        write!(f, "}};")
    }
}

impl HasSpan for Node {
    fn span(&self) -> Span {
        self.label
            .as_ref()
            .map(|lbl| lbl.span())
            .unwrap_or(self.name.span())
            .start()
            .to(self.payload.end.span.end())
    }
}

impl Display for Node {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(label) = &self.label {
            write!(f, "{}: ", label.item)?;
        }
        write!(f, "{} {}", self.name.item(), self.payload)
    }
}

#[derive(Eq, PartialEq, Debug)]
pub struct Memreserve {
    address: WithToken<u64>,
    length: WithToken<u64>,
}

impl Memreserve {
    pub fn new(address: WithToken<u64>, length: WithToken<u64>) -> Memreserve {
        Memreserve { address, length }
    }
}

impl Display for Memreserve {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "/memreserve/ 0x{:x} 0x{:x};",
            *self.address, *self.length
        )
    }
}

#[derive(Eq, PartialEq, Debug)]
pub struct DtsFile {
    pub elements: Vec<Primary>,
    pub source: Arc<StdPath>,
}

impl HasSource for DtsFile {
    fn source(&self) -> Arc<StdPath> {
        self.source.clone()
    }
}

impl Display for DtsFile {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        for primary in &self.elements {
            writeln!(f, "{primary}")?;
        }
        Ok(())
    }
}

#[derive(Eq, PartialEq, Debug, Clone)]
pub struct Include {
    pub include_token: Token,
    pub file_name: WithToken<String>,
}

impl HasSpan for Include {
    fn span(&self) -> Span {
        self.include_token.start().to(self.file_name.end())
    }
}

impl HasSource for Include {
    fn source(&self) -> Arc<StdPath> {
        self.include_token.source()
    }
}

impl Display for Include {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "/include/ \"{}\"", self.file_name)
    }
}

impl Include {
    pub fn file_name(&self) -> String {
        self.file_name.to_string()
    }
}

#[derive(Eq, PartialEq, Debug)]
pub enum AnyDirective {
    DtsHeader(Token),
    Plugin(Token),
    Memreserve(Memreserve),
    Include(Include),
    DeletedNode(Token, WithToken<Reference>),
    OmitIfNoRef(Token, WithToken<Reference>),
}

impl Display for AnyDirective {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            AnyDirective::DtsHeader(_) => write!(f, "/dts-v1/;"),
            AnyDirective::Memreserve(memreserve) => write!(f, "{memreserve};"),
            AnyDirective::Include(include) => write!(f, "{include}"),
            AnyDirective::Plugin(_) => write!(f, "/plugin/;"),
            AnyDirective::DeletedNode(_, reference) => write!(f, "/delete-node/ {reference};"),
            AnyDirective::OmitIfNoRef(_, reference) => write!(f, "/omit-if-no-ref/ {reference};"),
        }
    }
}

#[derive(Eq, PartialEq, Debug)]
pub struct ReferencedNode {
    pub label: Option<WithToken<String>>,
    pub reference: WithToken<Reference>,
    pub payload: NodePayload,
    pub span: Span,
}

impl HasSpan for ReferencedNode {
    fn span(&self) -> Span {
        self.reference.start().to(self.payload.end.end())
    }
}

impl Display for ReferencedNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.reference, self.payload)
    }
}

#[derive(Eq, PartialEq, Debug)]
pub enum Primary {
    Directive(AnyDirective),
    Root(Arc<Node>),
    ReferencedNode(Arc<ReferencedNode>),
    // C-style includes should be put into a separate pass
    CStyleInclude(String),
}

impl Primary {
    pub fn as_include(&self) -> Option<&Include> {
        match self {
            Primary::Directive(AnyDirective::Include(include)) => Some(include),
            _ => None,
        }
    }
}

impl Display for Primary {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Primary::Directive(directive) => write!(f, "{directive}"),
            Primary::Root(node) => write!(f, "{node}"),
            Primary::ReferencedNode(node) => write!(f, "{node}"),
            Primary::CStyleInclude(include) => write!(f, "#include {include}"),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn display_node_name() {
        let item = NodeName::with_address("foo", "12345678");
        assert_eq!(format!("{item}"), "foo@12345678");
    }

    #[test]
    fn display_absolute_path() {
        let item = AbsolutePath { elements: vec![] };
        assert_eq!(format!("{item}"), "/");

        let node = NodeName::with_address("foo", "12345678");
        let item = AbsolutePath {
            elements: vec![node],
        };
        assert_eq!(format!("{item}"), "/foo@12345678");

        let node1 = NodeName::with_address("foo", "12345678");
        let node2 = NodeName::simple("bar");
        let item = AbsolutePath {
            elements: vec![node1, node2],
        };
        assert_eq!(format!("{item}"), "/foo@12345678/bar");
    }

    #[test]
    fn display_dot_relative_path() {
        let item = DotRelativePath { elements: vec![] };
        assert_eq!(format!("{item}"), "./");

        let node = NodeName::with_address("foo", "12345678");
        let item = DotRelativePath {
            elements: vec![node],
        };
        assert_eq!(format!("{item}"), "./foo@12345678");

        let node1 = NodeName::with_address("foo", "12345678");
        let node2 = NodeName::simple("bar");
        let item = DotRelativePath {
            elements: vec![node1, node2],
        };
        assert_eq!(format!("{item}"), "./foo@12345678/bar");
    }

    #[test]
    fn display_label_relative_path() {
        let item = LabelRelativePath {
            label: "hello".into(),
            elements: vec![],
        };
        assert_eq!(format!("{item}"), "hello");

        let node = NodeName::with_address("foo", "12345678");
        let item = LabelRelativePath {
            label: "hello".into(),
            elements: vec![node],
        };
        assert_eq!(format!("{item}"), "hello/foo@12345678");

        let node1 = NodeName::with_address("foo", "12345678");
        let node2 = NodeName::simple("bar");
        let item = LabelRelativePath {
            label: "hello".into(),
            elements: vec![node1, node2],
        };
        assert_eq!(format!("{item}"), "hello/foo@12345678/bar");
    }

    #[test]
    fn display_path() {
        let item = Path::empty();
        assert_eq!(format!("{item}"), "/");

        let item = Path::new_dot_relative(vec![]);
        assert_eq!(format!("{item}"), "./");

        let item = Path::new_label_relative("hello".into(), vec![]);
        assert_eq!(format!("{item}"), "hello");
    }

    #[test]
    fn display_property_path() {
        let item = PropertyPath::new(Path::empty(), "prop".into());
        assert_eq!(format!("{item}"), "/prop");

        let item = PropertyPath::new(Path::new_dot_relative(vec![]), "prop".into());
        assert_eq!(format!("{item}"), "./prop");

        let item = PropertyPath::new(
            Path::new_label_relative("hello".into(), vec![]),
            "prop".into(),
        );
        assert_eq!(format!("{item}"), "hello/prop");

        let elements = [NodeName::simple("foo"), NodeName::simple("bar")];

        let item = PropertyPath::new(Path::new_absolute(elements.to_vec()), "prop".into());
        assert_eq!(format!("{item}"), "/foo/bar/prop");

        let item = PropertyPath::new(Path::new_dot_relative(elements.to_vec()), "prop".into());
        assert_eq!(format!("{item}"), "./foo/bar/prop");

        let item = PropertyPath::new(
            Path::new_label_relative("hello".into(), elements.to_vec()),
            "prop".into(),
        );
        assert_eq!(format!("{item}"), "hello/foo/bar/prop");
    }
}
