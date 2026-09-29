use crate::dts::ast::{AbsolutePath, NodeName, Property};
use crate::dts::{HasSpan, Node, Span};
use std::collections::HashMap;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::path::Path;
use std::sync::Arc;

pub struct Location {
    deleted: bool,
    file: Arc<Path>,
    span: Span,
}

impl Location {
    fn new(source: Arc<Path>, span: Span) -> Self {
        Location {
            deleted: false,
            file: source.clone(),
            span,
        }
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn span(&self) -> Span {
        self.span
    }
}

pub struct ResolvedProperty {
    deleted: bool,
    inner: Arc<Property>,
    sources: Vec<Location>,
}

#[derive(Default)]
pub struct ResolvedNode {
    deleted: bool,
    properties: HashMap<String, ResolvedProperty>,
    children: HashMap<NodeName, ResolvedNode>,
    sources: Vec<Location>,
}

impl ResolvedNode {
    pub fn active_child_nodes<'a>(&'a self) -> impl Iterator<Item = String> + use<'a> {
        self.children.iter().filter_map(|(name, child)| {
            if child.deleted {
                None
            } else {
                Some(name.to_string())
            }
        })
    }

    pub fn active_child_properties<'a>(
        &'a self,
    ) -> impl Iterator<Item = (String, Arc<Property>)> + use<'a> {
        self.properties.iter().filter_map(|(name, child)| {
            if child.deleted {
                None
            } else {
                Some((name.to_string(), child.inner.clone()))
            }
        })
    }

    pub fn sources<'a>(&'a self) -> impl Iterator<Item = &'a Location> + use<'a> {
        self.sources.iter().filter(|location| !location.deleted)
    }
}

#[derive(Default)]
pub struct ResolvedTree {
    root: ResolvedNode,
}

impl ResolvedTree {
    pub(crate) fn add_node(&mut self, path: &AbsolutePath, node: Arc<Node>, source: Arc<Path>) {
        let mut rnode = &mut self.root;
        for element in path.iter() {
            rnode = match rnode.children.entry(element.clone()) {
                Occupied(entry) => entry.into_mut(),
                Vacant(entry) => {
                    let child = ResolvedNode::default();
                    entry.insert(child)
                }
            }
        }
        rnode.sources.push(Location::new(source, node.span));
        rnode.deleted = false;
    }

    pub(crate) fn delete_node(&mut self, parent_path: &AbsolutePath, node_name: &NodeName) {
        let mut rparent = &mut self.root;
        for element in parent_path.iter() {
            rparent = match rparent.children.entry(element.clone()) {
                Occupied(entry) => entry.into_mut(),
                Vacant(entry) => {
                    let child = ResolvedNode::default();
                    entry.insert(child)
                }
            }
        }
        if let Some(rnode) = rparent.children.get_mut(node_name) {
            for source in rnode.sources.iter_mut() {
                source.deleted = true;
            }
            rnode.deleted = true;
        }
    }

    pub(crate) fn add_property(
        &mut self,
        path: &AbsolutePath,
        key: &str,
        value: Arc<Property>,
        source: Arc<Path>,
    ) {
        let mut rnode = &mut self.root;
        for element in path.iter() {
            rnode = rnode.children.get_mut(element).expect("proper tree");
        }
        let span = value.span();
        let rprop = ResolvedProperty {
            deleted: false,
            inner: value,
            sources: vec![Location::new(source, span)],
        };
        rnode.properties.insert(key.to_string(), rprop);
    }

    pub(crate) fn delete_property(&mut self, parent_path: &AbsolutePath, name: &str) {
        let mut rparent = &mut self.root;
        for element in parent_path.iter() {
            rparent = match rparent.children.entry(element.clone()) {
                Occupied(entry) => entry.into_mut(),
                Vacant(entry) => {
                    let child = ResolvedNode::default();
                    entry.insert(child)
                }
            }
        }
        if let Some(rprop) = rparent.properties.get_mut(name) {
            for source in rprop.sources.iter_mut() {
                source.deleted = true;
            }
            rprop.deleted = true;
        }
    }
}
