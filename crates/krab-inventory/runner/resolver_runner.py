"""Python resolver worker driven by the Rust inventory engine.

Loads a user `resolvers.py` the way kapitan's omegaconf backend did (import
the file, call `pass_resolvers()`, take the returned `{name: function}`
dict) and calls those functions on request. Speaks newline-delimited JSON on
stdin/stdout:

  {"op": "init", "id": 1, "file": "/abs/resolvers.py", "cwd": "/repo"}
    -> {"id": 1, "ok": true, "resolvers": {"name": {"wants": ["_root_"]}},
        "omegaconf": "2.3.0" | null}
  {"op": "call", "id": 2, "name": "sha256", "args": [...],
   "arg_kinds": ["literal", "node", "computed"],
   "node": {"key": "k", "full_key": "a.b[0]", "parent_key": "b",
            "parent_full_key": "a.b"}}
    -> {"id": 2, "ok": true, "value": ...}
    -> {"id": 2, "ok": false, "error": "ValueError: ...", "traceback": "..."}

While a call runs, `_root_` / `_parent_` lookups go back to the host as
`{"op": "select", "id": n, "key": "a.b", "relative": false}` and the host
answers `{"id": n, "ok": true, "found": true, "value": ...}` with the fully
resolved value (or `found: false`, or `ok: false` with an error).

Functions receive `_root_`, `_parent_` and `_node_` when their signature
names them, as with OmegaConf. `OmegaConf.select(_root_, key)`,
`OmegaConf.to_container(...)` and attribute/item access work on those
objects; when the omegaconf package is not installed a small stand-in module
provides the handful of `OmegaConf` functions resolvers typically use.
"""

import importlib.util
import inspect
import json
import os
import sys
import traceback
import types

RUNNER_PROTOCOL = 1

_MISSING = object()


# Replies go to a duplicate of fd 1. main() then points fd 1 at stderr, so a
# child process, C code or os.write(1, ...) cannot corrupt the protocol.
PROTO = sys.__stdout__


def respond(obj):
    PROTO.write(json.dumps(obj) + "\n")
    PROTO.flush()


class HostError(Exception):
    """The host could not answer a lookup (a nested resolver failed)."""


class Host:
    """Requests to the Rust side, answered on stdin while a call runs."""

    def __init__(self):
        self.next_id = 1

    def request(self, op, **fields):
        rid = self.next_id
        self.next_id += 1
        fields.update(op=op, id=rid)
        respond(fields)
        while True:
            line = sys.__stdin__.readline()
            if not line:
                raise HostError("host closed the connection")
            line = line.strip()
            if not line:
                continue
            reply = json.loads(line)
            if reply.get("id") != rid:
                raise HostError(f"out of order reply {reply!r}")
            if not reply.get("ok", False):
                raise HostError(reply.get("error", "lookup failed"))
            return reply

    def select(self, key, relative):
        reply = self.request("select", key=key, relative=relative)
        if not reply.get("found", False):
            return _MISSING
        return reply.get("value")


HOST = Host()


def _join(prefix, key):
    key = str(key)
    if prefix == "":
        return key
    if key.startswith("["):
        return prefix + key
    if prefix.endswith("."):
        return prefix + key
    return prefix + "." + key


class ConfigProxy:
    """Stands in for an OmegaConf container the Rust evaluator holds
    (`_root_`, `_parent_`). Lookups resolve on the Rust side and come back as
    plain, fully resolved values; containers are wrapped with
    `OmegaConf.create` when omegaconf is available."""

    def __init__(self, prefix, relative, key, full_key):
        self._prefix = prefix
        self._relative = relative
        self._key_value = key
        self._full_key = full_key

    # -- OmegaConf node API that resolvers use -------------------------------
    def _key(self):
        return self._key_value

    def _get_full_key(self, key=None):
        if key is None or key == "":
            return self._full_key
        return _join(self._full_key, key)

    def _select(self, key, default=_MISSING):
        value = HOST.select(_join(self._prefix, key), self._relative)
        if value is _MISSING:
            return None if default is _MISSING else default
        return _wrap(value)

    def _materialize(self):
        value = HOST.select(self._prefix, self._relative)
        return None if value is _MISSING else value

    # -- mapping / attribute access ------------------------------------------
    def _lookup(self, key):
        value = HOST.select(_join(self._prefix, key), self._relative)
        if value is _MISSING:
            raise KeyError(key)
        return _wrap(value)

    def __getattr__(self, name):
        if name.startswith("_"):
            raise AttributeError(name)
        try:
            return self._lookup(name)
        except KeyError:
            raise AttributeError(f"Missing key {name}") from None

    def __getitem__(self, key):
        return self._lookup(key)

    def get(self, key, default=None):
        try:
            return self._lookup(key)
        except KeyError:
            return default

    def __contains__(self, key):
        return HOST.select(_join(self._prefix, key), self._relative) is not _MISSING

    def _container(self):
        value = self._materialize()
        if not isinstance(value, (dict, list)):
            raise TypeError(f"not a container: {type(value).__name__}")
        return value

    def keys(self):
        return _as_dict(self._container()).keys()

    def values(self):
        return [_wrap(v) for v in _as_dict(self._container()).values()]

    def items(self):
        return [(k, _wrap(v)) for k, v in _as_dict(self._container()).items()]

    def __iter__(self):
        return iter(self._container())

    def __len__(self):
        return len(self._container())

    def __repr__(self):
        return f"<krab config proxy {self._full_key or '<root>'}>"


def _as_dict(value):
    if isinstance(value, dict):
        return value
    return {i: v for i, v in enumerate(value)}


class NodeProxy:
    """`_node_`: the node holding the interpolation being resolved."""

    def __init__(self, info, parent):
        self._info = info
        self._parent = parent

    def _key(self):
        return self._info.get("key")

    def _get_full_key(self, key=None):
        full = self._info.get("full_key") or ""
        if key is None or key == "":
            return full
        return _join(full, key)

    def _get_parent(self):
        return self._parent

    def __repr__(self):
        return f"<krab node proxy {self._get_full_key()}>"


# ---- omegaconf: patched when present, stood in for when absent -------------

OC = None  # the real OmegaConf class, when importable
OMEGACONF_VERSION = None


def _wrap(value):
    """Containers become OmegaConf configs when omegaconf is available."""
    if OC is not None and isinstance(value, (dict, list)):
        return OC.create(value)
    return value


def _install_omegaconf():
    global OC, OMEGACONF_VERSION
    try:
        import omegaconf
        from omegaconf import OmegaConf
    except ImportError:
        _install_shim()
        return
    OC = OmegaConf
    OMEGACONF_VERSION = getattr(omegaconf, "__version__", "unknown")

    real_select = OmegaConf.select
    real_to_container = OmegaConf.to_container
    real_is_config = OmegaConf.is_config
    real_is_dict = OmegaConf.is_dict
    real_is_list = OmegaConf.is_list

    def select(cfg, key, *args, **kwargs):
        if isinstance(cfg, ConfigProxy):
            default = kwargs.get("default", _MISSING)
            if args:
                default = args[0]
            return cfg._select(key, default)
        return real_select(cfg, key, *args, **kwargs)

    def to_container(cfg, *args, **kwargs):
        if isinstance(cfg, ConfigProxy):
            return cfg._materialize()
        return real_to_container(cfg, *args, **kwargs)

    def is_config(obj):
        return isinstance(obj, ConfigProxy) or real_is_config(obj)

    def is_dict(obj):
        if isinstance(obj, ConfigProxy):
            return isinstance(obj._materialize(), dict)
        return real_is_dict(obj)

    def is_list(obj):
        if isinstance(obj, ConfigProxy):
            return isinstance(obj._materialize(), list)
        return real_is_list(obj)

    OmegaConf.select = staticmethod(select)
    OmegaConf.to_container = staticmethod(to_container)
    OmegaConf.is_config = staticmethod(is_config)
    OmegaConf.is_dict = staticmethod(is_dict)
    OmegaConf.is_list = staticmethod(is_list)


def _install_shim():
    """A minimal `omegaconf` module: enough for `from omegaconf import
    Container, OmegaConf` and the calls resolvers make on `_root_`."""
    import copy

    class Container:  # noqa: D401 - marker type for annotations / isinstance
        """Stand-in for omegaconf.Container."""

    class DictConfig(Container):
        pass

    class ListConfig(Container):
        pass

    class OmegaConf:
        @staticmethod
        def select(cfg, key, *args, **kwargs):
            default = kwargs.get("default", _MISSING)
            if args:
                default = args[0]
            if isinstance(cfg, ConfigProxy):
                return cfg._select(key, default)
            cur = cfg
            for part in str(key).split("."):
                if isinstance(cur, dict) and part in cur:
                    cur = cur[part]
                elif isinstance(cur, list) and part.lstrip("-").isdigit() and -len(cur) <= int(part) < len(cur):
                    cur = cur[int(part)]
                else:
                    return None if default is _MISSING else default
            return cur

        @staticmethod
        def to_container(cfg, *args, **kwargs):
            if isinstance(cfg, ConfigProxy):
                return cfg._materialize()
            return copy.deepcopy(cfg)

        @staticmethod
        def create(obj=None, *args, **kwargs):
            return copy.deepcopy({} if obj is None else obj)

        @staticmethod
        def is_config(obj):
            return isinstance(obj, ConfigProxy)

        @staticmethod
        def is_dict(obj):
            if isinstance(obj, ConfigProxy):
                return isinstance(obj._materialize(), dict)
            return isinstance(obj, dict)

        @staticmethod
        def is_list(obj):
            if isinstance(obj, ConfigProxy):
                return isinstance(obj._materialize(), list)
            return isinstance(obj, list)

        @staticmethod
        def is_missing(cfg, key):
            return False

        @staticmethod
        def is_interpolation(node, key=None):
            return False

        @staticmethod
        def register_new_resolver(name, resolver, *, replace=False, use_cache=False):
            SHIM_REGISTERED[name] = resolver

        @staticmethod
        def has_resolver(name):
            return name in SHIM_REGISTERED

        @staticmethod
        def clear_resolver(name):
            return SHIM_REGISTERED.pop(name, None) is not None

    module = types.ModuleType("omegaconf")
    module.__krab_shim__ = True
    module.__version__ = "0.0.0+krab-shim"
    module.OmegaConf = OmegaConf
    module.Container = Container
    module.DictConfig = DictConfig
    module.ListConfig = ListConfig
    module.MISSING = "???"
    sys.modules["omegaconf"] = module


SHIM_REGISTERED = {}

# ---- the user's resolvers --------------------------------------------------

RESOLVERS = {}
SPECIALS = ("_root_", "_parent_", "_node_")


def _wants(func):
    try:
        params = inspect.signature(func).parameters
    except (TypeError, ValueError):
        return []
    return [p for p in SPECIALS if p in params]


def op_init(req):
    file = req["file"]
    if not os.path.isfile(file):
        raise FileNotFoundError(f"{file} does not exist")
    _install_omegaconf()
    for path in (os.path.dirname(os.path.abspath(file)), req.get("cwd")):
        if path and path not in sys.path:
            sys.path.append(path)
    spec = importlib.util.spec_from_file_location("resolvers", file)
    module = importlib.util.module_from_spec(spec)
    sys.modules["resolvers"] = module
    spec.loader.exec_module(module)
    pass_resolvers = getattr(module, "pass_resolvers", None)
    if not callable(pass_resolvers):
        raise ImportError(f"{file} must define a function pass_resolvers()")
    funcs = pass_resolvers()
    if not isinstance(funcs, dict):
        raise TypeError(f"pass_resolvers() should return a dict, got {type(funcs).__name__}")
    RESOLVERS.clear()
    listing = {}
    for name, func in funcs.items():
        if not callable(func):
            raise TypeError(f"resolver {name!r} is not callable")
        RESOLVERS[str(name)] = func
        listing[str(name)] = {"wants": _wants(func)}
    return {
        "ok": True,
        "protocol": RUNNER_PROTOCOL,
        "resolvers": listing,
        "modules": _project_modules(os.path.realpath(file)),
        "omegaconf": OMEGACONF_VERSION,
        "python": sys.version.split()[0],
    }


def _project_modules(resolver_file):
    """Files of the modules the import loaded that are neither the standard
    library nor installed packages: the project's own code, whose change
    should trigger a fresh import."""
    import sysconfig

    roots = set()
    for name in ("stdlib", "platstdlib", "purelib", "platlib"):
        try:
            roots.add(os.path.realpath(sysconfig.get_paths()[name]) + os.sep)
        except (KeyError, TypeError):
            pass
    for prefix in (sys.prefix, sys.base_prefix, sys.exec_prefix):
        roots.add(os.path.realpath(prefix) + os.sep)
    roots.add(os.path.realpath(os.path.dirname(__file__)) + os.sep)
    out = set()
    for mod in list(sys.modules.values()):
        f = getattr(mod, "__file__", None)
        if not isinstance(f, str):
            continue
        real = os.path.realpath(f)
        if real == resolver_file or "site-packages" in real or "dist-packages" in real:
            continue
        if any(real.startswith(r) for r in roots):
            continue
        out.add(real)
    return sorted(out)


def _to_plain(value, where="resolver result"):
    """Convert what a resolver returned into JSON-serialisable plain data,
    with the type rules OmegaConf applies to resolver output."""
    if isinstance(value, float) and (value != value or value in (float("inf"), float("-inf"))):
        raise TypeError(f"non-finite float {value!r} in {where}")
    if value is None or isinstance(value, (bool, int, float, str)):
        return value
    if OC is not None and OC.is_config(value) and not isinstance(value, ConfigProxy):
        return _to_plain(OC.to_container(value, resolve=True), where)
    if isinstance(value, ConfigProxy):
        return value._materialize()
    if isinstance(value, dict):
        return {str(k): _to_plain(v, where) for k, v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_to_plain(v, where) for v in value]
    import enum
    import pathlib

    if isinstance(value, enum.Enum):
        return _to_plain(value.value, where)
    if isinstance(value, pathlib.PurePath):
        return str(value)
    raise TypeError(f"unsupported type {type(value).__name__} in {where}")


def op_call(req):
    name = req["name"]
    func = RESOLVERS.get(name)
    if func is None:
        raise KeyError(f"unknown resolver {name!r}")
    node = req.get("node") or {}
    args = list(req.get("args") or [])
    kinds = req.get("arg_kinds") or []
    for i, value in enumerate(args):
        kind = kinds[i] if i < len(kinds) else "computed"
        if kind != "literal" and isinstance(value, (dict, list)):
            args[i] = _wrap(value)
    wants = _wants(func)
    kwargs = {}
    parent = ConfigProxy(".", True, node.get("parent_key"), node.get("parent_full_key") or "")
    if "_root_" in wants:
        kwargs["_root_"] = ConfigProxy("", False, None, "")
    if "_parent_" in wants:
        kwargs["_parent_"] = parent
    if "_node_" in wants:
        kwargs["_node_"] = NodeProxy(node, parent)
    result = func(*args, **kwargs)
    return {"ok": True, "value": _to_plain(result)}


def main():
    global PROTO
    PROTO = os.fdopen(os.dup(1), "w")
    os.dup2(2, 1)
    sys.stdout = sys.stderr  # user code prints must not corrupt the protocol
    while True:
        line = sys.__stdin__.readline()
        if not line:
            return
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            op = req.get("op")
            if op == "init":
                result = op_init(req)
            elif op == "call":
                result = op_call(req)
            elif op == "exit":
                respond({"id": req.get("id"), "ok": True})
                return
            else:
                result = {"ok": False, "error": f"unknown op {op!r}"}
        except HostError as e:
            result = {"ok": False, "error": str(e), "host_error": True}
        except Exception as e:  # noqa: BLE001
            result = {
                "ok": False,
                "error": f"{type(e).__name__}: {e}",
                "traceback": traceback.format_exc(),
            }
        result["id"] = req.get("id") if isinstance(req, dict) else None
        respond(result)


if __name__ == "__main__":
    main()
