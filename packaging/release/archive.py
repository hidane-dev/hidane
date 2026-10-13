#!/usr/bin/env python3
"""Packs a release directory into its archive, so that the same files give the same bytes.

    SOURCE_DATE_EPOCH=<seconds> python3 packaging/release/archive.py <directory> <archive>

<archive> ends in .tar.gz or .zip and holds <directory> under its own name. The entries are
sorted by path; each is dated SOURCE_DATE_EPOCH (the release workflow uses the commit's time) and
owned by 0:0 without user or group names; directories and the `hidane` binary get mode 0755,
everything else 0644. The tar is ustar, compressed by gzip with that same time and no file name
in its header. Only the zlib version can still change the compressed bytes, so archives are
compared between builds on the same runner image (docs/reproducible-builds.md).
"""

import gzip
import os
import sys
import tarfile
import time
import zipfile

EXECUTABLES = {"hidane", "hidane.exe"}


def entries(directory):
    """(archive path, file system path, is a directory), the directory first, sorted by path."""
    base = os.path.basename(os.path.normpath(directory))
    found = [(base, directory, True)]
    for parent, dirs, files in os.walk(directory):
        dirs.sort()
        rel = os.path.relpath(parent, directory)
        prefix = base if rel == "." else base + "/" + rel.replace(os.sep, "/")
        found += [(f"{prefix}/{d}", os.path.join(parent, d), True) for d in dirs]
        found += [(f"{prefix}/{f}", os.path.join(parent, f), False) for f in files]
    return sorted(found)


def mode(name, is_dir):
    return 0o755 if is_dir or name.rsplit("/", 1)[-1] in EXECUTABLES else 0o644


def write_tar_gz(directory, archive, epoch):
    with open(archive, "wb") as raw, gzip.GzipFile(
        filename="", mode="wb", fileobj=raw, mtime=epoch, compresslevel=9
    ) as gz, tarfile.open(fileobj=gz, mode="w", format=tarfile.USTAR_FORMAT) as tar:
        for name, path, is_dir in entries(directory):
            info = tarfile.TarInfo(name)
            info.mtime = epoch
            info.mode = mode(name, is_dir)
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            if is_dir:
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            else:
                info.size = os.path.getsize(path)
                with open(path, "rb") as f:
                    tar.addfile(info, f)


def write_zip(directory, archive, epoch):
    stamp = time.gmtime(epoch)[:6]
    with zipfile.ZipFile(archive, "w") as z:
        for name, path, is_dir in entries(directory):
            info = zipfile.ZipInfo(name + "/" if is_dir else name, date_time=stamp)
            info.create_system = 3  # Unix, so that the modes below are read
            if is_dir:
                info.external_attr = (0o040000 | mode(name, True)) << 16 | 0x10
                z.writestr(info, b"")
            else:
                info.external_attr = (0o100000 | mode(name, False)) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                with open(path, "rb") as f:
                    z.writestr(info, f.read(), compresslevel=9)


def main():
    if len(sys.argv) != 3:
        sys.exit("usage: SOURCE_DATE_EPOCH=<seconds> archive.py <directory> <archive>")
    directory, archive = sys.argv[1:]
    if not os.path.isdir(directory):
        sys.exit(f"archive.py: {directory} is not a directory")
    try:
        epoch = int(os.environ["SOURCE_DATE_EPOCH"])
    except (KeyError, ValueError):
        sys.exit("archive.py: set SOURCE_DATE_EPOCH to the time to date the entries with")
    if archive.endswith(".tar.gz"):
        write_tar_gz(directory, archive, epoch)
    elif archive.endswith(".zip"):
        write_zip(directory, archive, epoch)
    else:
        sys.exit(f"archive.py: {archive} ends in neither .tar.gz nor .zip")


if __name__ == "__main__":
    main()
