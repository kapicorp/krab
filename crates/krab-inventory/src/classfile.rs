//! The shape of a class or target file: `classes`, `parameters`,
//! `applications`, `exports`.

use crate::error::{Error, Result};
use crate::source::Origin;
use crate::value::{Map, Node, Value};

#[derive(Clone, Debug)]
pub struct ClassRef {
    pub name: String,
    pub origin: Origin,
}

#[derive(Clone, Debug)]
pub struct ClassDoc {
    pub classes: Vec<ClassRef>,
    /// Always a map (possibly empty).
    pub parameters: Node,
    pub applications: Vec<Node>,
    /// Always a map (possibly empty).
    pub exports: Node,
}

impl ClassDoc {
    /// Interpret a parsed YAML document. Mirrors the reference loader:
    /// `null` sections are treated as empty, unknown top-level keys are ignored.
    pub fn from_node(doc: Node) -> Result<ClassDoc> {
        let origin = doc.origin;
        let map = match doc.value {
            Value::Map(m) => m,
            Value::Null => Map::new(),
            other => {
                return Err(Error::new(
                    "inventory::bad_document",
                    format!(
                        "expected a mapping at the top of the file, found {}",
                        other.type_name()
                    ),
                )
                .with_label(origin, "not a mapping"));
            }
        };
        let mut classes = Vec::new();
        let mut parameters = Node::map(origin);
        let mut applications = Vec::new();
        let mut exports = Node::map(origin);
        for (key, node) in map {
            match key.as_str() {
                "classes" => match node.value {
                    // `_classes = content.get("classes") or []` in the
                    // reference: anything falsy means no classes, and an empty
                    // mapping is what a commented-out class list leaves behind.
                    ref v if !v.truthy() => {}
                    Value::List(items) => {
                        for item in items {
                            match item.value {
                                Value::Str(name) => classes.push(ClassRef {
                                    name,
                                    origin: item.origin,
                                }),
                                other => {
                                    return Err(Error::new(
                                        "inventory::bad_class_ref",
                                        format!(
                                            "class names must be strings, found {}",
                                            other.type_name()
                                        ),
                                    )
                                    .with_label(item.origin, "not a string"));
                                }
                            }
                        }
                    }
                    other => {
                        return Err(Error::new(
                            "inventory::bad_classes",
                            format!("`classes` must be a list, found {}", other.type_name()),
                        )
                        .with_label(node.origin, "not a list"));
                    }
                },
                "parameters" => match node.value {
                    Value::Null => {}
                    Value::Map(_) => parameters = node,
                    other => {
                        return Err(Error::new(
                            "inventory::bad_parameters",
                            format!(
                                "`parameters` must be a mapping, found {}",
                                other.type_name()
                            ),
                        )
                        .with_label(node.origin, "not a mapping"));
                    }
                },
                "applications" => match node.value {
                    Value::Null => {}
                    Value::List(items) => applications = items,
                    other => {
                        return Err(Error::new(
                            "inventory::bad_applications",
                            format!("`applications` must be a list, found {}", other.type_name()),
                        )
                        .with_label(node.origin, "not a list"));
                    }
                },
                "exports" => match node.value {
                    Value::Null => {}
                    Value::Map(_) => exports = node,
                    other => {
                        return Err(Error::new(
                            "inventory::bad_exports",
                            format!("`exports` must be a mapping, found {}", other.type_name()),
                        )
                        .with_label(node.origin, "not a mapping"));
                    }
                },
                _ => {}
            }
        }
        Ok(ClassDoc {
            classes,
            parameters,
            applications,
            exports,
        })
    }
}
