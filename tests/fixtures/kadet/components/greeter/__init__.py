"""A kadet component using the API kgenlib-style code imports from `kapitan`."""

import sys

from kapitan.inputs.kadet import BaseObj, inventory, load_from_search_paths
from kapitan.resources import inventory as all_documents

greetlib = load_from_search_paths("greetlib")


def main(input_params):
    inv = inventory()
    greeting = greetlib.Greeting(text=f"{inv.parameters.greeting} from {inv.parameters.name}")
    out = BaseObj()
    out.root.greeting = greeting
    out.root.rendered = greetlib.render("templates/banner.j2", {"name": inv.parameters.name, "bits": ["a", "b"]})
    out.root.api_replicas = greetlib.other_replicas("app.api")
    out.root.ports = greetlib.all_ports()
    out.root.target = greetlib.me()
    out.root.pruned = greetlib.prune_empty({"keep": 1, "drop": [], "nested": {"drop": {}}})
    out.root.targets = sorted(all_documents(None).keys())
    out.root.params = input_params.get("flavour")
    # A write to an element of `a + b` lands in `a`, as with kapitan's python-box (#320).
    pod = BaseObj().root
    pod.containers = [{"name": "app"}]
    pod.initContainers = []
    for container in pod.containers + pod.initContainers:
        container["securityContext"] = {"readOnlyRootFilesystem": True}
    out.root.pod = pod
    # Nothing from an installed kapitan may have been imported.
    out.root.kapitan_modules = sorted(m for m in sys.modules if m == "kapitan" or m.startswith("kapitan."))
    return out
