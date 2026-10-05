"""Regenerate expected/*.yaml with the reference kapitan (run with PEX_INTERPRETER=1)."""
import os, sys
sys.argv = ["kapitan"]
import yaml, kapitan.cli  # noqa: F401  (registers representers)
from kapitan.inventory.backends.omegaconf import OmegaConfInventory
from kapitan.utils import PrettyDumper
here = os.path.dirname(os.path.abspath(__file__))
inv = OmegaConfInventory(inventory_path=os.path.join(here, "inventory"), compose_target_name=True, enable_class_wildcards=True)
os.makedirs(os.path.join(here, "expected"), exist_ok=True)
for name in inv.targets:
    with open(os.path.join(here, "expected", name + ".yaml"), "w") as f:
        yaml.dump(inv.inventory[name], f, Dumper=PrettyDumper, default_flow_style=False, indent=2)
print("wrote", len(inv.targets), "targets")
