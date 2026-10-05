"""Locates the krab binary this package installed."""

import os
import sys
import sysconfig


def find_krab_bin():
    """Return the path of the krab binary installed with this package."""
    here = os.path.dirname(os.path.abspath(__file__))
    dirs = [
        sysconfig.get_path("scripts"),
        sysconfig.get_path("scripts", vars={"base": sys.base_prefix}),
        # pip install --prefix: <prefix>/lib/pythonX.Y/site-packages/krab
        os.path.join(here, "..", "..", "..", "..", "bin"),
        # pip install --target: <target>/krab next to <target>/bin
        os.path.join(here, "..", "bin"),
        sysconfig.get_path("scripts", scheme=sysconfig.get_preferred_scheme("user")),
    ]
    for d in dirs:
        path = os.path.join(d, "krab")
        if os.path.isfile(path):
            return os.path.normpath(path)
    raise FileNotFoundError("no krab binary in " + ", ".join(os.path.normpath(d) for d in dirs))
