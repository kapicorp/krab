//! With `enable-class-wildcards`, a class file added or removed under
//! `classes/` re-renders the targets whose patterns can match it (#69).

use std::path::PathBuf;
use std::sync::Arc;

use krab_inventory::resolvers::Registry;
use krab_inventory::{Inventory, InventoryConfig};
use krab_server::State;

fn classes(state: &State) -> Vec<String> {
    state.read().targets["t"].classes.clone()
}

#[test]
fn a_matching_class_file_added_or_removed_re_renders() {
    let root: PathBuf = std::env::temp_dir().join(format!("krab-wildcards-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("targets")).unwrap();
    std::fs::create_dir_all(root.join("classes/comp")).unwrap();
    std::fs::write(root.join("classes/comp/a.yml"), "parameters: {}\n").unwrap();
    // The pattern sits in a class, so its cached closure must be dropped too.
    std::fs::write(root.join("classes/all.yml"), "classes: [comp.*]\n").unwrap();
    std::fs::write(root.join("targets/t.yml"), "classes: [all]\n").unwrap();
    let mut cfg = InventoryConfig::new(root.clone());
    cfg.class_wildcards = true;
    let state = State::new(Inventory::new(cfg, Arc::new(Registry::with_builtins())));
    state.render_all();
    assert_eq!(classes(&state), ["comp.a", "all"]);

    let added = root.join("classes/comp/b/init.yml");
    std::fs::create_dir_all(added.parent().unwrap()).unwrap();
    std::fs::write(&added, "parameters: {}\n").unwrap();
    state.apply_changes(vec![added.clone()]);
    assert_eq!(classes(&state), ["comp.a", "comp.b", "all"]);

    std::fs::remove_file(&added).unwrap();
    state.apply_changes(vec![added]);
    assert_eq!(classes(&state), ["comp.a", "all"]);
    let _ = std::fs::remove_dir_all(&root);
}
