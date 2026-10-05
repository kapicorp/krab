//! The `.kapitan` settings file (YAML) found in the working directory.

use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::source::SourceId;
use crate::value::{Map, Node, Value};
use crate::yaml::parse_document;

/// The `inventory.python-resolvers` section: a user `resolvers.py` run in a
/// Python worker (see `resolvers::python`). Either a path, `false`, or a map:
///
/// ```yaml
/// inventory:
///   python-resolvers:
///     file: system/omegaconf/resolvers/resolvers.py   # default: the reference's discovery
///     python: /opt/venv/bin/python                     # $KRAB_PYTHON overrides; default: a kapitan PEX, python3
///     prefer-native: true                              # keep native resolvers over same-named Python ones
///     workers: 4
///     enabled: true
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PythonResolverSettings {
    pub enabled: Option<bool>,
    pub file: Option<PathBuf>,
    pub python: Option<String>,
    pub prefer_native: Option<bool>,
    pub workers: Option<usize>,
}

impl PythonResolverSettings {
    fn from_node(node: &Node) -> PythonResolverSettings {
        let mut s = PythonResolverSettings::default();
        match &node.value {
            Value::Str(path) => s.file = Some(PathBuf::from(path)),
            Value::Bool(b) => s.enabled = Some(*b),
            Value::Map(m) => {
                let str_of = |k: &str| m.get(k).and_then(|n| n.as_str()).map(str::to_string);
                let bool_of = |k: &str| match m.get(k).map(|n| &n.value) {
                    Some(Value::Bool(b)) => Some(*b),
                    _ => None,
                };
                s.file = str_of("file").or_else(|| str_of("path")).map(PathBuf::from);
                s.python = str_of("python");
                s.enabled = bool_of("enabled");
                s.prefer_native = bool_of("prefer-native");
                s.workers = match m.get("workers").map(|n| &n.value) {
                    Some(Value::Int(i)) if *i > 0 => Some(*i as usize),
                    _ => None,
                };
            }
            _ => {}
        }
        s
    }
}

#[derive(Clone, Debug, Default)]
pub struct DotKapitan {
    pub inventory_path: Option<PathBuf>,
    pub compose_target_name: Option<bool>,
    pub inventory_backend: Option<String>,
    /// An `inventory_backend:` section without the `inventory-backend` key,
    /// the only key kapitan reads from it.
    pub legacy_backend_key: bool,
    pub indent: Option<usize>,
    pub python_resolvers: PythonResolverSettings,
    pub file: Option<PathBuf>,
    /// The raw `compile:` section, keys as written (`search-paths`, `output-path`, ...).
    pub compile: Map,
    /// The raw `inventory:` section.
    pub inventory: Map,
    /// The raw `refs:` section.
    pub refs: Map,
    /// The raw `global:` section, the fallback for every other section.
    pub global: Map,
}

impl DotKapitan {
    /// Look for `.kapitan` in `dir`; absent file yields defaults.
    pub fn load(dir: &Path) -> Result<DotKapitan> {
        let file = dir.join(".kapitan");
        if !file.is_file() {
            return Ok(DotKapitan::default());
        }
        let text = std::fs::read_to_string(&file)?;
        let node = parse_document(&text, SourceId::SYNTHETIC)?;
        let mut cfg = DotKapitan {
            file: Some(file),
            compile: node
                .get("compile")
                .and_then(Node::as_map)
                .cloned()
                .unwrap_or_default(),
            inventory: node
                .get("inventory")
                .and_then(Node::as_map)
                .cloned()
                .unwrap_or_default(),
            refs: node
                .get("refs")
                .and_then(Node::as_map)
                .cloned()
                .unwrap_or_default(),
            global: node
                .get("global")
                .and_then(Node::as_map)
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        };
        let section = |name: &str| node.get(name).and_then(Node::as_map);
        let get = |sections: &[&str], key: &str| -> Option<Value> {
            sections
                .iter()
                .find_map(|s| section(s).and_then(|m| m.get(key)).map(|n| n.value.clone()))
        };
        if let Some(Value::Str(s)) = get(&["compile", "inventory", "global"], "inventory-path") {
            cfg.inventory_path = Some(PathBuf::from(s));
        }
        if let Some(Value::Bool(b)) = get(&["compile", "inventory", "global"], "compose-node-name")
            .or_else(|| get(&["compile", "inventory", "global"], "compose-target-name"))
        {
            cfg.compose_target_name = Some(b);
        }
        // kapitan: `from_dot_kapitan("inventory_backend", "inventory-backend", ...)`,
        // the `inventory_backend` section first, then `global`.
        if let Some(Value::Str(b)) = get(&["inventory_backend", "global"], "inventory-backend") {
            cfg.inventory_backend = Some(b);
        }
        cfg.legacy_backend_key =
            section("inventory_backend").is_some_and(|m| m.get("inventory-backend").is_none());
        if let Some(Value::Int(i)) = get(&["inventory", "global"], "indent") {
            cfg.indent = Some(i.max(1) as usize);
        }
        if let Some(n) = cfg.inventory.get("python-resolvers") {
            cfg.python_resolvers = PythonResolverSettings::from_node(n);
        }
        Ok(cfg)
    }

    /// Why kapitan would not render this inventory with omegaconf, the only
    /// backend krab implements.
    pub fn backend_warning(&self) -> Option<String> {
        let what = match self.inventory_backend.as_deref() {
            Some("omegaconf") => return None,
            Some(other) => format!("`.kapitan` selects the `{other}` inventory backend"),
            None if self.file.is_none() => "there is no `.kapitan`".to_string(),
            None => "`.kapitan` sets no `inventory-backend`".to_string(),
        };
        let legacy = if self.legacy_backend_key {
            " (the `inventory_backend` section has no `inventory-backend` key, the only one kapitan reads there)"
        } else {
            ""
        };
        Some(format!(
            "{what}{legacy}, so kapitan renders this inventory with reclass; krab implements omegaconf only and its output can differ. Set `global.inventory-backend: omegaconf`"
        ))
    }

    /// kapitan's `from_dot_kapitan`: the command's section, then `global`.
    fn setting<'a>(&'a self, section: &'a Map, key: &str) -> Option<&'a Node> {
        section.get(key).or_else(|| self.global.get(key))
    }

    /// A string list from the compile section (`search-paths`).
    pub fn compile_strings(&self, key: &str) -> Option<Vec<String>> {
        match &self.setting(&self.compile, key)?.value {
            Value::List(l) => Some(
                l.iter()
                    .filter_map(|n| n.as_str().map(str::to_string))
                    .collect(),
            ),
            Value::Str(s) => Some(vec![s.clone()]),
            _ => None,
        }
    }

    pub fn compile_str(&self, key: &str) -> Option<String> {
        self.setting(&self.compile, key)
            .and_then(|n| n.as_str())
            .map(str::to_string)
    }

    pub fn compile_bool(&self, key: &str) -> Option<bool> {
        match self.setting(&self.compile, key)?.value {
            Value::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn compile_int(&self, key: &str) -> Option<i64> {
        match self.setting(&self.compile, key)?.value {
            Value::Int(i) => Some(i),
            _ => None,
        }
    }

    pub fn inventory_str(&self, key: &str) -> Option<String> {
        self.setting(&self.inventory, key)
            .and_then(|n| n.as_str())
            .map(str::to_string)
    }

    /// A string from the refs section (`refs-path`).
    pub fn refs_str(&self, key: &str) -> Option<String> {
        self.setting(&self.refs, key)
            .and_then(|n| n.as_str())
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_compile_section() {
        let dir = std::env::temp_dir().join(format!("dotkapitan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".kapitan"),
            "version: 0.36\nglobal:\n  inventory-backend: omegaconf\ncompile:\n  prune: true\n  indent: 4\n  search-paths:\n    - .\n    - ./system/\ninventory:\n  multiline-string-style: literal\n",
        )
        .unwrap();
        let dot = DotKapitan::load(&dir).unwrap();
        assert_eq!(
            dot.compile_strings("search-paths"),
            Some(vec![".".to_string(), "./system/".to_string()])
        );
        assert_eq!(dot.compile_bool("prune"), Some(true));
        assert_eq!(dot.compile_int("indent"), Some(4));
        assert_eq!(
            dot.inventory_str("multiline-string-style").as_deref(),
            Some("literal")
        );
        assert_eq!(dot.python_resolvers, PythonResolverSettings::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn section_keys_fall_back_to_global() {
        let dir = std::env::temp_dir().join(format!("dotkapitan-global-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".kapitan"),
            "global:\n  output-path: out\n  prune: true\n  indent: 4\n  search-paths: [lib]\n  refs-path: secrets\n  multiline-string-style: literal\ncompile:\n  output-path: build\n",
        )
        .unwrap();
        let dot = DotKapitan::load(&dir).unwrap();
        // The command's section wins over `global`.
        assert_eq!(dot.compile_str("output-path").as_deref(), Some("build"));
        assert_eq!(dot.compile_bool("prune"), Some(true));
        assert_eq!(dot.compile_int("indent"), Some(4));
        assert_eq!(
            dot.compile_strings("search-paths"),
            Some(vec!["lib".to_string()])
        );
        assert_eq!(dot.compile_str("refs-path").as_deref(), Some("secrets"));
        assert_eq!(dot.indent, Some(4));
        assert_eq!(
            dot.inventory_str("multiline-string-style").as_deref(),
            Some("literal")
        );
        assert_eq!(dot.refs_str("refs-path").as_deref(), Some("secrets"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reads_python_resolvers_section() {
        let dir = std::env::temp_dir().join(format!("dotkapitan-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".kapitan"),
            "inventory:\n  python-resolvers:\n    file: system/resolvers.py\n    python: /opt/venv/bin/python\n    prefer-native: true\n    workers: 2\n",
        )
        .unwrap();
        let dot = DotKapitan::load(&dir).unwrap();
        assert_eq!(
            dot.python_resolvers,
            PythonResolverSettings {
                enabled: None,
                file: Some(PathBuf::from("system/resolvers.py")),
                python: Some("/opt/venv/bin/python".into()),
                prefer_native: Some(true),
                workers: Some(2),
            }
        );
        std::fs::write(
            dir.join(".kapitan"),
            "inventory:\n  python-resolvers: false\n",
        )
        .unwrap();
        let dot = DotKapitan::load(&dir).unwrap();
        assert_eq!(dot.python_resolvers.enabled, Some(false));
        std::fs::write(
            dir.join(".kapitan"),
            "inventory:\n  python-resolvers: lib/resolvers.py\n",
        )
        .unwrap();
        let dot = DotKapitan::load(&dir).unwrap();
        assert_eq!(
            dot.python_resolvers.file,
            Some(PathBuf::from("lib/resolvers.py"))
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn warns_unless_omegaconf_is_selected() {
        let dir = std::env::temp_dir().join(format!("dotkapitan-backend-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let warning = |text: Option<&str>| {
            let _ = std::fs::remove_file(dir.join(".kapitan"));
            if let Some(t) = text {
                std::fs::write(dir.join(".kapitan"), t).unwrap();
            }
            DotKapitan::load(&dir).unwrap().backend_warning()
        };
        let none = warning(None).unwrap();
        assert!(none.contains("inventory-backend: omegaconf"), "{none}");
        assert_eq!(
            warning(Some("global:\n  inventory-backend: omegaconf\n")),
            None
        );
        // kapitan reads `inventory_backend.inventory-backend`, then `global`.
        assert_eq!(
            warning(Some("inventory_backend:\n  inventory-backend: omegaconf\n")),
            None
        );
        assert!(warning(Some("compile:\n  inventory-backend: omegaconf\n")).is_some());
        let overridden = warning(Some(
            "inventory_backend:\n  inventory-backend: reclass\nglobal:\n  inventory-backend: omegaconf\n",
        ))
        .unwrap();
        assert!(overridden.contains("`reclass`"), "{overridden}");
        let reclass = warning(Some("global:\n  inventory-backend: reclass\n")).unwrap();
        assert!(reclass.contains("`reclass`"), "{reclass}");
        let legacy = warning(Some("inventory_backend:\n  omegaconf: true\n")).unwrap();
        assert!(
            legacy.contains("`inventory_backend` section has no `inventory-backend` key"),
            "{legacy}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
