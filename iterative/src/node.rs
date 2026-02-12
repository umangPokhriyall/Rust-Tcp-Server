// node.rs
use crate::router::HandlerFn;

pub struct Node {
    pub key: String,
    pub children: Vec<Node>,
    pub handler: Option<HandlerFn>,
}

impl Node {
    pub fn new(key: &str) -> Self {
        Self {
            key: key.to_string(),
            children: Vec::new(),
            handler: None,
        }
    }

    pub fn insert(&mut self, path: &str, handler: HandlerFn) {
        let path = path.trim_start_matches('/');

        if path.is_empty() {
            self.handler = Some(handler);
            return;
        }

        let mut parts = path.splitn(2, '/');
        let segment = parts.next().unwrap();
        let rest = parts.next().unwrap_or("");

        if let Some(child) = self.children.iter_mut().find(|n| n.key == segment) {
            child.insert(rest, handler);
        } else {
            let mut new_node = Node::new(segment);
            new_node.insert(rest, handler);
            self.children.push(new_node);
        }
    }

    pub fn get(&self, path: &str) -> Option<HandlerFn> {
        let path = path.trim_start_matches('/');

        if path.is_empty() {
            return self.handler;
        }

        let mut parts = path.splitn(2, '/');
        let segment = parts.next().unwrap();
        let rest = parts.next().unwrap_or("");

        self.children
            .iter()
            .find(|n| n.key == segment)
            .and_then(|child| child.get(rest))
    }
}
