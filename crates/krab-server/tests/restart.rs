//! A change to a file the server was configured from (`.kapitan`, a
//! `resolvers.py`) makes it stop, so the next request starts a fresh one;
//! an inventory file change re-renders in place.

use std::path::PathBuf;
use std::sync::Arc;

use krab_inventory::resolvers::Registry;
use krab_inventory::{Inventory, InventoryConfig};
use krab_server::State;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

#[test]
fn a_changed_configuration_source_stops_the_server() {
    let dot = fixtures().join(".kapitan");
    let mut registry = Registry::with_builtins();
    registry.add_source(dot.clone());
    let inventory = fixtures().join("inventory");
    let state = State::new(Inventory::new(
        InventoryConfig::new(inventory.clone()),
        Arc::new(registry),
    ));
    state.render_all();
    assert!(!state.should_stop());

    state.apply_changes(vec![inventory.join("targets")]);
    assert!(
        !state.should_stop(),
        "an inventory change re-renders in place"
    );

    state.apply_changes(vec![dot]);
    assert!(
        state.should_stop(),
        "a .kapitan change must restart the server"
    );
}
