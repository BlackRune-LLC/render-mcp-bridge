"""Read a pinned GitHub node archive without importing or executing package code."""

import json
import re
import shutil
import stat
import struct
import sys
import unicodedata
import zipfile
from pathlib import Path

MAX_ENTRIES = 10_000
MAX_UNPACKED_BYTES = 256 * 1024 * 1024
MAX_ENTRY_BYTES = 64 * 1024 * 1024
RESERVED = {"con", "prn", "aux", "nul", "conin$", "conout$"}
RESERVED.update(f"{prefix}{number}" for prefix in ("com", "lpt") for number in "123456789¹²³")
PROTECTED = {".git", ".render-bridge-node-pack.json", "__pycache__"}
DEPENDENCY_NAMES = {
    "pyproject.toml", "setup.py", "setup.cfg", "environment.yml", "environment.yaml",
    "package.json", "install.py", "install.sh", "install.bat", "install.ps1",
}


def check_component(part):
    folded = unicodedata.normalize("NFC", part).casefold()
    if (
        not part
        or part in {".", ".."}
        or part.endswith((" ", "."))
        or any(unicodedata.category(char).startswith("C") or char in '<>:"\\|?*' for char in part)
        or folded in PROTECTED
        or folded.partition(".")[0] in RESERVED
        or len(part.encode("utf-8")) > 240
    ):
        raise ValueError("unsafe archive path")
    return folded


def check_archive_index(path):
    # Bound the central directory before ZipFile allocates one object per entry.
    end_record = struct.Struct("<4s4H2LH")
    with path.open("rb") as archive:
        size = archive.seek(0, 2)
        tail_size = min(size, 65535 + end_record.size)
        archive.seek(size - tail_size)
        tail = archive.read(tail_size)
    index = tail.rfind(b"PK\x05\x06")
    if index < 0 or len(tail) - index < end_record.size:
        raise ValueError("archive index missing")
    _, disk, index_disk, disk_entries, entries, index_size, start, comment = end_record.unpack_from(tail, index)
    if (
        disk
        or index_disk
        or disk_entries != entries
        or not 0 < entries <= MAX_ENTRIES
        or index_size > 8 * 1024 * 1024
        or start + index_size != size - tail_size + index
        or index + end_record.size + comment != len(tail)
    ):
        raise ValueError("unsupported archive index")
    return entries


def review_archive(archive, target, expected_root):
    names = {}
    files = set()
    dependencies = []
    hooks = []
    total = 0
    entry_count = check_archive_index(archive)
    with zipfile.ZipFile(archive) as package:
        members = package.infolist()
        if len(members) != entry_count:
            raise ValueError("archive size")
        for member in members:
            kind = stat.S_IFMT(member.external_attr >> 16)
            directory = member.is_dir()
            if (
                member.orig_filename != member.filename
                or kind not in (0, stat.S_IFDIR if directory else stat.S_IFREG)
                or member.flag_bits & 1
                or member.external_attr & 0x400
                or member.file_size > MAX_ENTRY_BYTES
            ):
                raise ValueError("unsupported archive member")
            total += member.file_size
            if total > MAX_UNPACKED_BYTES:
                raise ValueError("extracted archive size")
            root, *parts = member.filename.removesuffix("/").split("/")
            if root.casefold() != expected_root.casefold() or (not parts and not directory):
                raise ValueError("archive commit does not match")
            if not parts:
                continue
            if len(parts) > 20 or sum(map(len, parts)) > 360:
                raise ValueError("archive path too long")
            normalized = tuple(map(check_component, parts))
            for length in range(1, len(parts) + 1):
                prefix = normalized[:length]
                actual = tuple(parts[:length])
                if names.setdefault(prefix, actual) != actual:
                    raise ValueError("ambiguous archive path")
            destination = target.joinpath(*parts)
            if directory:
                destination.mkdir(parents=True, exist_ok=True)
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            with package.open(member) as source, destination.open("xb") as output:
                shutil.copyfileobj(source, output)
            files.add(tuple(parts))
            if len(parts) != 1:
                continue
            name = normalized[0]
            if name in DEPENDENCY_NAMES or re.fullmatch(r"requirements[^/]*\.txt", name):
                dependencies.append(parts[0])
            if name in ("prestartup_script.py", "install.py"):
                hooks.append(parts[0])

    if ("__init__.py",) not in files:
        raise ValueError("missing ComfyUI package entry point")
    return {"files": len(files), "dependencies": sorted(dependencies), "startup_hooks": sorted(hooks)}


if __name__ == "__main__":
    archive, target, expected_root = sys.argv[1:]
    review = review_archive(Path(archive), Path(target), expected_root)
    print(json.dumps(review))
