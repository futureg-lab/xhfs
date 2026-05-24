# XHFS (Extended Headless File System)

XHFS is a small, full-fledged file system that enables you to store files
anywhere, on anything, across an arbitrary combination of arbitrary devices.

# Concept

Fundamentally, any writable, readable, and seekable entity can be used as a
storage device, whether or not it was built for such an operation: be it a file
on disk, a remote Key-Value store (Redis, Cloudflare KV), or hell, even a
database.

XHFS makes no assumptions about what the underlying "device" actually is; in
fact, it remains entirely agnostic as long as you provide a way to read/write to
arbitrary locations.

While its core design is inspired by ext4, the primary architectural split is
that its inode extents are organized as a linked list instead of a H Tree, which
you may argue makes it slower for seek operations but still good enough.

# Use-cases

- Basic: You can setup blob file replicas across drives on top of your local
  filesystem.
- As a storage layer on top of an existing one:
  - Split your XHFS storage into multiple files, then store them on a locally
    synchronized folder using
    [null.fs](https://github.com/michael-0acf4/null.fs), Syncthing, Google
    Drive, Mega, OneDrive or anything similar. You get an encrypted storage
    layer that can be shared publicly + automatic backups.
- Distributed: Out of the box, XHFS makes no assumptions about the underlying
  device as long as we can read/write/seek arbitrary data.
- As Yet Another Filesystem: The wiring is already there, you can hack your way
  into formatting a physical block device or write a driver for it.

# Example

Consider the following configuration:

```yaml
# xhfs.yaml
# Encrypt the drive
# or use the cli with --password helloworld if the config and bins are made public
password: helloworld
devices:
  - type: file
    name: blob1
    path: ./part1.bin
  - type: file
    name: blob2
    path: ./part1-replica.bin
  - type: file
    name: blob3
    path: ./part2.bin
configuration:
  logical:
    - name: dev1
      include: [blob1, blob2]
      capacity: "50 MiB"
      max_concurrent: 2
    - name: dev2
      include: [blob3]
      capacity: "50 MiB"
      max_concurrent: 1
  # Final storage layout
  # [dev1: 0 - 50MB] [dev2: 50MiB - 100MiB]
  layout: [dev1, dev2]
```

All that is left to do is format the drive (if you haven't done so yet) and then
start playing with it.

```bash
# Setup the File System if not formatted yet
xhfs format

echo "Hello World" > thething.txt
xhfs upload thething.txt /test.txt

xhfs x ls -v
# FILE 2026-05-14 16:49:49        28 B test.txt

xhfs x read test.txt | echo
# Hello World

# You can also import stored files like this..
xhfs download test.txt my_physical_copy.txt
```

# Installation

Download the binary from the
[release](https://github.com/futureg-lab/xhfs/releases).

## Environment variables

- `XHFS_CONFIG`: Can be overridden by `--config <CONFIG>`. If none is set, will
  default to `xhfs.yaml`.
- `XHFS_PASSWORD`: Can be overridden by password in config file if set or
  `--password`. The xhfs client will not use encryption if all are unset (you
  will still need a password for an existing encrypted volume though).
- XHFS will automatically load from .env if detected.

# Features

- [x] File System
  - [x] Encryption (ChaCha20 stream cipher)
  - [x] No journaling, CoW based
  - [x] xhfs core: fopen, fwrite, fseek, mkdir, fmove, fcopy, unlink
  - [x] Native Symlink support: create_symlink
  - [x] Native Hardlink support: create_hardlink
  - [x] Extra: fappend
- [ ] RAID-like configuration
  - [x] Logical grouping
  - [x] Replication
  - [ ] Error correction
- [ ] No device assumption
  - [x] File device
  - [x] In memory device
  - [x] Custom KV http endpoint
  - [x] Cloudflare KV example
  - [ ] s3 device
- [x] Explorer
  - [x] CLI
  - [x] Inspection utilities: `xhfs inspect`
  - [x] Servers
    - [x] Webdav server: `xhfs server webdav -p 1144`

# Inspection tools

The command line provides a few sets of utilities you can use to inspect the
formatted filesystem.

## General state

```bash
xhfs info
```

The `info` command will show you the general layout of what constitutes your
storage, this includes the remaining usable space and metadata layout.

```
XHFS version: 1
Capacity:     4194304 B
Remaining:    3790848 B
Format Configuration:
  Block Size:       1024 B
  Blocks per Group: 4096
  Total Groups:     1
Geometry Layout (relative):
  Group Stride:        4194304 B
  Inodes per Group:    8192
  Usable Blocks/Group: 3702
  Header Region:       0x00000000 -- 0x00000028 (        41 B)
  Data Bitmap Region:  0x00000029 -- 0x00000230 (       520 B)
  INode Bitmap Region: 0x00000231 -- 0x00000638 (      1032 B)
  INode Table Region:  0x00000639 -- 0x00062638 (    401408 B)
  Data Payload Region: 0x00062639 -- 0x003ffe38 (   3790848 B)
```

## INode metadata

```bash
# Display metadata of an INode and its extent address
xhfs inspect inode /Pictures/cat.jpg
# INode #4
# - Number of Links: 1
# - Kind: File
# - Size: 1050318 B
# - Creation time: 2026-05-17 21:26:29 UTC
# - Modification time: 2026-05-17 21:26:29 UTC
# - Immediate Extent address: 404025 (0x00062a39)

# then the Extent chain up to a count
xhfs inspect extent 0x00062a39 -m 3 
# #1 :: 0x00062a39 -- 0x00062e39 (      1025 B)
# #2 :: 0x00164239 -- 0x00264639 (   1049601 B)
# #3 :: 0x00264639 -- 0x00264e39 (      2049 B)
```

## Block view and dump

If you want to read from the filesystem directly, you can dump or view its
content either raw or decrypted. This can be useful if you want to make custom a
tool that reconstruct removed file extents, dump the entire filesystem content
locally or inspect the state of the data.

```bash
# which you can view in hex (don't forget to decrypt for the data and INode regions)
xhfs inspect view 0x00062a39 0x00062a50 -c 16 --decrypt
# 00000000: 00 00 00 00 00 00 00 00 39 42 16 00 00 00 00 00 | ........9B......
# 00000010: 01 00 00 00 00 00 00                            | .......

# or even dump
xhfs inspect dump 0x00062a39 0x00062a50 stuff.bin --decrypt

# and many other things too...
```
