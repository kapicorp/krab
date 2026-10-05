"""kadet evaluator for the native `krab compile`.

The only part of a compile that has to run Python: importing a kadet
component (``__init__.py`` under the input path) and calling its ``main()``.
The result is returned as JSON; formatting, ref embedding and writing happen
in Rust. Every file, directory listing, module and target document the
component read is reported so the compile can be skipped next time.

The component's ``kapitan.*`` imports resolve to the package next to this
file (see ``kapitan/__init__.py``), configured with the documents and
settings the host sends; the Python kapitan is not needed.

Protocol: newline-delimited JSON on stdin/stdout.
  {"op": "init", "cwd": ..., "inventory_file": ... | "inventory_socket": ...,
   "settings": {...}, "krab_version": ...}
  {"op": "eval", "target": ..., "input_path": ..., "input_params": {...},
   "compile_path": ..., "temp_dir": ...}
  {"op": "exit"}
While an eval runs the evaluator may ask the host for things the same way
(a line with an ``op`` and an ``id`` on stdout, the answer on stdin):
  {"op": "helm", "chart_dir": ..., "helm_params": {...}, "helm_values_file": ..., "parse": bool}
"""

import builtins
import inspect
import io
import json
import os
import sys
import traceback

PROTOCOL = 4
HERE = os.path.dirname(os.path.abspath(__file__))


class InventoryClient:
    """Fetches rendered target documents from the krab daemon (JSON-RPC over
    a unix socket) on demand, so an evaluator only receives the targets a
    component actually reads."""

    def __init__(self, socket_path):
        import socket

        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(socket_path)
        self.fp = self.sock.makefile("rwb")
        self.next_id = 1

    def call(self, method, params):
        req = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}
        self.next_id += 1
        self.fp.write((json.dumps(req) + "\n").encode())
        self.fp.flush()
        line = self.fp.readline()
        if not line:
            raise RuntimeError("inventory server closed the connection")
        resp = json.loads(line)
        if resp.get("error"):
            raise KeyError(resp["error"].get("message", "inventory server error"))
        return resp["result"]

    def target(self, name):
        return self.call("inventory.target", {"name": name})["document"]

    def names(self):
        return [t["name"] for t in self.call("inventory.targets", None)["targets"] if t.get("ok")]

    def all(self):
        return self.call("inventory.all", None)["documents"]


class LazyDocs(dict):
    """Target documents, fetched from the server when first needed. Behaves
    like the plain dict of every rendered target."""

    def __init__(self, client):
        super().__init__()
        self._client = client
        self._names = None
        self._complete = False

    def _load_all(self):
        if not self._complete:
            for name, doc in self._client.all().items():
                dict.__setitem__(self, name, doc)
            self._complete = True

    def _name_list(self):
        if self._names is None:
            self._names = self._client.names()
        return self._names

    def __getitem__(self, key):
        if not dict.__contains__(self, key):
            try:
                doc = self._client.target(key)
            except KeyError:
                raise KeyError(key) from None
            dict.__setitem__(self, key, doc)
        return dict.__getitem__(self, key)

    def get(self, key, default=None):
        try:
            return self[key]
        except KeyError:
            return default

    def __contains__(self, key):
        return dict.__contains__(self, key) or key in self._name_list()

    def __iter__(self):
        return iter(self._name_list())

    def __len__(self):
        return len(self._name_list())

    def keys(self):
        return list(self._name_list())

    def values(self):
        self._load_all()
        return dict.values(self)

    def items(self):
        self._load_all()
        return dict.items(self)


# Replies go to a duplicate of fd 1. main() then points fd 1 at stderr, so a
# child process, C code or os.write(1, ...) cannot corrupt the protocol.
PROTO = sys.__stdout__


def respond(obj):
    PROTO.write(json.dumps(obj, default=str) + "\n")
    PROTO.flush()


class HostError(Exception):
    """The host refused or failed a request."""


HOST_IDS = iter(range(1, sys.maxsize))


def host_call(op, params):
    """Ask the host (the compiler reading our stdout) for something in the
    middle of an evaluation; it answers on our stdin."""
    respond({"op": op, "id": next(HOST_IDS), **params})
    line = sys.__stdin__.readline()
    if not line:
        raise RuntimeError("host closed the connection")
    resp = json.loads(line)
    if not resp.get("ok"):
        raise HostError(resp.get("error") or f"host request {op!r} failed")
    return resp


class Recorder:
    """What an evaluation depended on: files and directory listings under
    the repository, other targets' documents, and parts of the target's own
    document. Fed by the I/O hooks below and by the `kapitan` package."""

    def __init__(self, root):
        self.root = os.path.realpath(root) + os.sep
        self.active = False
        self.target = None
        self.reset()

    def reset(self):
        self.files, self.dirs, self.globals, self.doc_reads = set(), set(), set(), set()

    def _real(self, path):
        try:
            real = os.path.realpath(path)
        except (TypeError, ValueError):
            return None
        return real if real.startswith(self.root) else None

    def file(self, path):
        if self.active and (real := self._real(path)):
            self.files.add(real)

    def dir(self, path):
        if self.active and (real := self._real(path)):
            self.dirs.add(real)

    def global_target(self, key):
        """Target `key` (or `*`, all of them) was read through the global
        inventory. Reading the current target that way counts as all of its
        document."""
        if key == self.target:
            self.doc_read("*")
        elif self.active:
            self.globals.add(key if isinstance(key, str) else "*")

    def doc_read(self, key):
        """`parameters.<key>`, another top-level key, or `*` of the current
        target's document was read."""
        if self.active:
            self.doc_reads.add(key)

    def modules(self):
        out = set()
        for mod in list(sys.modules.values()):
            f = getattr(mod, "__file__", None)
            if isinstance(f, str) and (real := self._real(f)):
                out.add(real)
        return out


RECORDER = None
STATE = {}


def install_hooks(recorder):
    real_open, real_io_open = builtins.open, io.open
    real_scandir, real_listdir = os.scandir, os.listdir

    def is_read(mode):
        return not any(c in mode for c in "wax+")

    def rec_open(file, mode="r", *a, **kw):
        if isinstance(file, (str, bytes, os.PathLike)) and is_read(mode):
            recorder.file(os.fsdecode(file))
        return real_open(file, mode, *a, **kw)

    def rec_io_open(file, mode="r", *a, **kw):
        if isinstance(file, (str, bytes, os.PathLike)) and is_read(mode):
            recorder.file(os.fsdecode(file))
        return real_io_open(file, mode, *a, **kw)

    def rec_scandir(path=".", *a, **kw):
        if isinstance(path, (str, bytes, os.PathLike)):
            recorder.dir(os.fsdecode(path))
        return real_scandir(path, *a, **kw)

    def rec_listdir(path=".", *a, **kw):
        if isinstance(path, (str, bytes, os.PathLike)):
            recorder.dir(os.fsdecode(path))
        return real_listdir(path, *a, **kw)

    builtins.open, io.open = rec_open, rec_io_open
    os.scandir, os.listdir = rec_scandir, rec_listdir

    import importlib._bootstrap_external as bootstrap
    import importlib.util

    real_get_data = bootstrap.FileLoader.get_data

    def rec_get_data(self, path):
        p = os.fsdecode(path)
        if p.endswith(".pyc"):
            try:
                p = importlib.util.source_from_cache(p)
            except ValueError:
                pass
        recorder.file(p)
        return real_get_data(self, path)

    bootstrap.FileLoader.get_data = rec_get_data


def import_bundled_kapitan():
    """Make `import kapitan` resolve to the package next to this script,
    ahead of any kapitan installed in this Python."""
    if HERE not in sys.path:
        sys.path.insert(0, HERE)
    other = sys.modules.get("kapitan")
    if other is not None and not getattr(other, "BUNDLED", False):
        raise RuntimeError(
            f"a kapitan package was imported before the evaluator started ({getattr(other, '__file__', '?')}); "
            "the evaluator needs its own"
        )
    import kapitan

    if not getattr(kapitan, "BUNDLED", False):
        raise RuntimeError(f"`import kapitan` resolved to {kapitan.__file__}, not the package under {HERE}")
    return kapitan


def op_init(req):
    global RECORDER
    os.chdir(req["cwd"])
    sys.path.insert(0, req["cwd"])
    RECORDER = Recorder(req["cwd"])
    install_hooks(RECORDER)

    import logging

    logging.basicConfig(level=logging.WARNING, stream=sys.stderr, format="%(levelname)s %(name)s: %(message)s")

    import_bundled_kapitan()
    import kadet
    from kapitan import runtime
    from kapitan.inputs import kadet as kadet_input

    if req.get("inventory_socket"):
        docs = LazyDocs(InventoryClient(req["inventory_socket"]))
    else:
        with open(req["inventory_file"]) as fp:
            docs = json.load(fp)
    settings = runtime.Settings(**(req.get("settings") or {}))
    runtime.configure(
        documents_=docs,
        settings_=settings,
        recorder_=RECORDER,
        helm=lambda request: host_call("helm", request),
        version=req.get("krab_version"),
    )
    STATE["search_paths"] = [os.path.abspath(p) for p in settings.search_paths]
    STATE["kadet_input"] = kadet_input
    try:
        from importlib.metadata import version

        kadet_version = version("kadet")
    except Exception:  # noqa: BLE001 - kadet without package metadata (a checkout on PYTHONPATH)
        kadet_version = getattr(kadet, "__version__", "unknown")
    return {
        "ok": True,
        "kadet_version": kadet_version,
        "python": sys.version.split()[0],
        "protocol": PROTOCOL,
    }


def op_eval(req):
    from kapitan.runtime import current_target

    kadet_input = STATE["kadet_input"]
    target = req["target"]
    input_path = req["input_path"]
    input_params = dict(req.get("input_params") or {})
    input_params.setdefault("compile_path", req["compile_path"])
    RECORDER.reset()
    RECORDER.active = True
    RECORDER.target = target
    token = current_target.set(target)
    try:
        kadet_input.search_paths.set(STATE["search_paths"] + [req["temp_dir"]])
        module, spec = kadet_input.module_from_path(input_path)
        sys.modules[spec.name] = module
        spec.loader.exec_module(module)
        argspec = inspect.getfullargspec(module.main)
        if len(argspec.args) > 1:
            raise ValueError(f"Kadet {spec.name} main parameters not equal to 1 or 0")
        output = module.main(input_params) if len(argspec.args) == 1 else module.main()
        output = kadet_input._to_dict(output)
        return {
            "ok": True,
            "output": output,
            "files": sorted(RECORDER.files | RECORDER.modules()),
            "dirs": sorted(RECORDER.dirs),
            "globals": sorted(RECORDER.globals),
            "doc_reads": sorted(RECORDER.doc_reads),
        }
    except Exception as e:  # noqa: BLE001
        hint = kadet_input.missing_package_hint(e) if "kgenlib" not in str(e) and "search paths" not in str(e) else ""
        return {
            "ok": False,
            "error": f"Could not load Kadet module: {os.path.basename(input_path)}: {e}{hint}",
            "traceback": traceback.format_exc(),
        }
    finally:
        RECORDER.active = False
        RECORDER.target = None
        current_target.reset(token)


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
            elif op == "eval":
                result = op_eval(req)
            elif op == "exit":
                respond({"id": req.get("id"), "ok": True})
                return
            else:
                result = {"ok": False, "error": f"unknown op {op!r}"}
        except Exception as e:  # noqa: BLE001
            result = {"ok": False, "error": str(e), "traceback": traceback.format_exc()}
        result["id"] = req.get("id") if isinstance(req, dict) else None
        respond(result)


if __name__ == "__main__":
    main()
