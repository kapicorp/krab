"""Compile worker driven by the Rust `kapitan compile`.

Speaks newline-delimited JSON on stdin/stdout. Reuses kapitan's own input
types and output writers so compiled files are byte-identical to the Python
implementation, and records every input the compile touched (files read,
directories listed, Python modules loaded from the repository, targets read
from the global inventory) so the Rust side can decide later whether the
target needs compiling again.
"""

import builtins
import io
import json
import os
import sys
import time
import traceback

RUNNER_PROTOCOL = 1


# Replies go to a duplicate of fd 1. main() then points fd 1 at stderr, so a
# child process, C code or os.write(1, ...) cannot corrupt the protocol.
PROTO = sys.__stdout__


def respond(obj):
    PROTO.write(json.dumps(obj) + "\n")
    PROTO.flush()


class Recorder:
    """Collects the inputs one target compile touched."""

    def __init__(self, root):
        self.root = os.path.realpath(root) + os.sep
        self.files = set()
        self.dirs = set()
        self.globals = set()
        self.active = False

    def under_root(self, path):
        try:
            real = os.path.realpath(path)
        except (TypeError, ValueError):
            return None
        if real.startswith(self.root):
            return real
        return None

    def file(self, path):
        if self.active and (real := self.under_root(path)):
            self.files.add(real)

    def dir(self, path):
        if self.active and (real := self.under_root(path)):
            self.dirs.add(real)

    def reset(self):
        self.files, self.dirs, self.globals = set(), set(), set()

    def modules(self):
        out = set()
        for mod in list(sys.modules.values()):
            f = getattr(mod, "__file__", None)
            if isinstance(f, str) and (real := self.under_root(f)):
                out.add(real)
        return out


RECORDER = None


def install_hooks(recorder):
    real_open = builtins.open
    real_io_open = io.open
    real_scandir = os.scandir
    real_listdir = os.listdir

    def rec_open(file, mode="r", *a, **kw):
        if isinstance(file, (str, bytes, os.PathLike)) and not any(c in mode for c in "wax+"):
            recorder.file(os.fsdecode(file))
        return real_open(file, mode, *a, **kw)

    def rec_io_open(file, mode="r", *a, **kw):
        if isinstance(file, (str, bytes, os.PathLike)) and not any(c in mode for c in "wax+"):
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

    builtins.open = rec_open
    io.open = rec_io_open
    os.scandir = rec_scandir
    os.listdir = rec_listdir

    # The import system reads sources through the loader, not `open`; a
    # cached .pyc means the .py is never even read, so map it back.
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


TRACE_GLOBALS = os.environ.get("KRAB_RUNNER_TRACE_GLOBALS") == "1"


def note_global(recorder, key):
    """Record a read of the global inventory; `*` means everything."""
    if not recorder.active:
        return
    key = key if isinstance(key, str) else "*"
    if TRACE_GLOBALS and key == "*" and "*" not in recorder.globals:
        sys.stderr.write("global inventory iterated from:\n" + "".join(traceback.format_stack(limit=12)[:-1]))
    recorder.globals.add(key)


class RecordingGlobalInventory(dict):
    """`cached.global_inv` stand-in: notes which targets were read."""

    def __init__(self, data, recorder):
        super().__init__(data)
        self._recorder = recorder

    def _note(self, key):
        note_global(self._recorder, key)

    def __getitem__(self, key):
        self._note(key)
        return super().__getitem__(key)

    def get(self, key, default=None):
        self._note(key)
        return super().get(key, default)

    def __contains__(self, key):
        self._note(key)
        return super().__contains__(key)

    def __iter__(self):
        self._note("*")
        return super().__iter__()

    def keys(self):
        self._note("*")
        return super().keys()

    def values(self):
        self._note("*")
        return super().values()

    def items(self):
        self._note("*")
        return super().items()

    def __len__(self):
        self._note("*")
        return super().__len__()


class FakeTarget:
    """Enough of `kapitan.inventory.InventoryTarget` for generator code."""

    def __init__(self, name, doc):
        self.name = name
        self._doc = doc
        self.parameters = doc.get("parameters") or {}
        self.classes = doc.get("classes") or []
        self.applications = doc.get("applications") or []
        self.exports = doc.get("exports") or {}

    def model_dump(self, *args, **kwargs):
        return self._doc


class FakeInventory(dict):
    """What kapitan code expects in `cached.inv`: target docs by name plus the
    parts of `kapitan.inventory.Inventory` used at compile time (`targets`,
    `inventory`, `get_target`, topics)."""

    @property
    def inventory(self):
        return dict(self)

    @property
    def targets(self):
        return {name: FakeTarget(name, doc) for name, doc in dict.items(self)}

    def get_target(self, name, *args, **kwargs):
        doc = dict.get(self, name)
        return FakeTarget(name, doc) if doc is not None else None

    def get_targets(self, names=None, *args, **kwargs):
        targets = self.targets
        if names:
            return {n: targets[n] for n in names if n in targets}
        return targets

    def get_parameters(self, names, *args, **kwargs):
        if isinstance(names, str):
            return (dict.get(self, names) or {}).get("parameters")
        return {n: {"parameters": (dict.get(self, n) or {}).get("parameters")} for n in names}

    @property
    def topics(self):
        topics = {}
        for name, doc in dict.items(self):
            kap = (doc.get("parameters") or {}).get("kapitan") or {}
            for topic, values in (kap.get("topics") or {}).items():
                params = values.get("parameters") if isinstance(values, dict) else None
                if params is None:
                    continue
                topics.setdefault(topic, {})[name] = params
        return {n: {"parameters": {"targets": t}} for n, t in topics.items()}

    def consumed_topics(self, target):
        doc = dict.get(self, target) or {}
        kap = (doc.get("parameters") or {}).get("kapitan") or {}
        return {n for n, v in (kap.get("topics") or {}).items() if isinstance(v, dict) and v.get("consume") is True}


STATE = {}


def op_init(req):
    """Load the inventory, build kapitan's compile args, wire up global state."""
    global RECORDER
    os.chdir(req["cwd"])
    sys.path.insert(0, req["cwd"])
    RECORDER = Recorder(req["cwd"])
    install_hooks(RECORDER)

    import logging

    logging.basicConfig(level=logging.WARNING, stream=sys.stderr, format="%(levelname)s %(name)s: %(message)s")

    from kapitan import cached
    from kapitan.cli import build_parser
    from kapitan.refs.base import RefController, Revealer
    from kapitan.version import VERSION

    with open(req["inventory_file"]) as fp:
        docs = json.load(fp)
    args = build_parser().parse_args(["compile", *req.get("flags", [])])
    cached.args = args
    cached.inv = FakeInventory(docs)
    cached.global_inv = RecordingGlobalInventory(docs, RECORDER)
    ref_controller = RefController(args.refs_path, embed_refs=args.embed_refs)
    cached.ref_controller_obj = ref_controller
    cached.revealer_obj = Revealer(ref_controller)

    # kadet's inventory_global() wraps cached.global_inv in a kadet.Dict once;
    # make it a recording one so cross-target reads are tracked.
    import kadet
    import kapitan.inputs.kadet as kadet_input

    roots = set()

    class RecordingDict(kadet.Dict):
        """The global inventory Dict: records which targets are read. Box
        builds nested values with this same class, so only the root instances
        record anything; Box also calls `__getitem__` with extra keyword
        arguments internally."""

        def _note(self, key):
            if id(self) in roots:
                note_global(RECORDER, key)

        def __getitem__(self, key, *args, **kwargs):
            self._note(key)
            return super().__getitem__(key, *args, **kwargs)

        def get(self, key, *args, **kwargs):
            self._note(key)
            return super().get(key, *args, **kwargs)

        def __contains__(self, key):
            self._note(key)
            return super().__contains__(key)

        def __iter__(self):
            self._note("*")
            return super().__iter__()

        def keys(self, *args, **kwargs):
            self._note("*")
            return super().keys(*args, **kwargs)

        def values(self, *args, **kwargs):
            self._note("*")
            return super().values(*args, **kwargs)

        def items(self, *args, **kwargs):
            self._note("*")
            return super().items(*args, **kwargs)

        def to_dict(self, *args, **kwargs):
            self._note("*")
            return super().to_dict(*args, **kwargs)

    recording_global = {}

    def inventory_global(lazy=False):
        if lazy not in recording_global:
            root = RecordingDict(dict(docs), default_box=lazy)
            roots.add(id(root))
            recording_global[lazy] = root
        return recording_global[lazy]

    kadet_input.inventory_global = inventory_global

    # kapitan's kadet output cache must stay off: a hit would skip the generator
    # and hide the files it reads from our dependency recording. The helm render
    # cache (keyed on chart contents and values) stays on; it is what makes
    # chart-heavy generators fast.
    kadet_input.Kadet.cacheable = lambda self: False

    STATE["args"] = args
    STATE["ref_controller"] = ref_controller
    STATE["search_paths"] = [os.path.abspath(p) for p in args.search_paths]
    try:
        from kapitan.yaml_ryml import HAS_RYML
    except Exception:  # pragma: no cover
        HAS_RYML = False
    return {
        "ok": True,
        "kapitan_version": VERSION,
        "python": sys.version.split()[0],
        "rapidyaml": bool(HAS_RYML),
        "protocol": RUNNER_PROTOCOL,
    }


def op_compile(req):
    from kapitan.errors import CompileError
    from kapitan.inputs import get_compiler
    from kapitan.inventory.model.input_types import CompileInputTypeConfig
    from pydantic import TypeAdapter

    adapter = TypeAdapter(CompileInputTypeConfig)
    target = req["target"]
    args = STATE["args"]
    # kapitan appends the run's temp directory to the search paths so inputs can
    # refer to output compiled earlier in the same run; here it is per target.
    search_paths = STATE["search_paths"] + [req["temp_dir"]]
    start = time.time()
    RECORDER.reset()
    RECORDER.active = True
    errors = []
    try:
        for item in req["compile"]:
            cfg = adapter.validate_python(item)
            try:
                compiler = get_compiler(cfg.input_type)(
                    req["compile_path"], search_paths, STATE["ref_controller"], target, args
                )
                compiler.compile_obj(cfg)
            except Exception as e:  # noqa: BLE001
                if cfg.continue_on_compile_error:
                    errors.append(f"{cfg.input_type} {list(cfg.input_paths)}: {e}")
                    continue
                raise CompileError(f"{cfg.input_type} {list(cfg.input_paths)}: {e}") from e
    except Exception as e:  # noqa: BLE001
        RECORDER.active = False
        return {
            "ok": False,
            "error": str(e),
            "traceback": traceback.format_exc(),
            "ms": int((time.time() - start) * 1000),
        }
    RECORDER.active = False
    return {
        "ok": True,
        "files": sorted(RECORDER.files | RECORDER.modules()),
        "dirs": sorted(RECORDER.dirs),
        "globals": sorted(RECORDER.globals),
        "warnings": errors,
        "ms": int((time.time() - start) * 1000),
    }


def main():
    global PROTO
    PROTO = os.fdopen(os.dup(1), "w")
    os.dup2(2, 1)
    # Anything user code prints must not corrupt the protocol stream.
    sys.stdout = sys.stderr
    for line in sys.__stdin__:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
            op = req.get("op")
            if op == "init":
                result = op_init(req)
            elif op == "compile":
                result = op_compile(req)
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
