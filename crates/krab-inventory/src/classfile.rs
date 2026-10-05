//! The shape of a class or target file: `classes`, `parameters`,
//! `applications`, `exports`.

use crate::error::{Diagnostic, Error, Result};
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
    /// One `inventory::unknown_section` warning per other top-level key.
    pub warnings: Vec<Diagnostic>,
}

const SECTIONS: [&str; 4] = ["classes", "parameters", "applications", "exports"];

/// Levenshtein distance, for "did you mean".
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = (prev + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            prev = row[j + 1];
            row[j + 1] = cur;
        }
    }
    row[b.len()]
}

impl ClassDoc {
    /// Interpret a parsed YAML document. Mirrors the reference loader:
    /// `null` sections are treated as empty, unknown top-level keys are ignored
    /// with a warning.
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
        let mut warnings = Vec::new();
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
                _ => {
                    let help = match SECTIONS.iter().find(|s| distance(&key, s) <= 2) {
                        Some(s) => format!("did you mean `{s}`?"),
                        None => format!("a class or target file reads {}", SECTIONS.join(", ")),
                    };
                    warnings.push(
                        Diagnostic::warning(
                            "inventory::unknown_section",
                            format!(
                                "unknown top-level key `{key}` is ignored, as kapitan ignores it"
                            ),
                        )
                        .with_label(node.origin, "value ignored")
                        .with_help(help),
                    );
                }
            }
        }
        Ok(ClassDoc {
            classes,
            parameters,
            applications,
            exports,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceId;
    use crate::yaml::parse_document;

    #[test]
    fn an_unknown_top_level_key_warns_with_a_suggestion() {
        let text = "paramters:\n  replicas: 3\nfoo: 1\nparameters:\n  a: 1\n";
        let doc = ClassDoc::from_node(parse_document(text, SourceId::SYNTHETIC).unwrap()).unwrap();
        assert!(doc.parameters.get("a").is_some());
        let [typo, other] = doc.warnings.as_slice() else {
            panic!("{:?}", doc.warnings)
        };
        assert_eq!(typo.code, "inventory::unknown_section");
        assert!(typo.message.contains("`paramters`"), "{}", typo.message);
        assert_eq!(typo.help.as_deref(), Some("did you mean `parameters`?"));
        assert!(other.message.contains("`foo`"), "{}", other.message);
        assert_eq!(
            other.help.as_deref(),
            Some("a class or target file reads classes, parameters, applications, exports")
        );
    }
}
