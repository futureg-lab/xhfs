# brutefs

A tiny, full-fledged file system that enables you to store files anywhere, on
anything, across an arbitrary combination of devices.

# Concept

Fundamentally, any writable, readable, and seekable entity can be used as a
storage device, whether or not it was built for such an operation: be it a file
on disk, a remote Key-Value store (Redis, Cloudflare KV), or hell, even a
database.

brutefs makes no assumptions about what the underlying "device" actually is; in
fact, it remains entirely agnostic as long as you provide a way to read/write to
arbitrary locations.

While its core design is inspired by ext4, the primary architectural split is
that its inode extents are organized as a linked list instead of a H Tree, which
you may argue makes it slower for seek operations but still good enough for
checking correctness. The mechanism it uses to reclaim and reuse freed memory to
mitigate fragmentation is also quite different.

# Example

Consider the following configuration:

```yaml
# brutefs.yaml
password: helloworld
devices:
    - type: file
      name: bloc1
      path: ./part1.bin
    - type: file
      name: bloc2
      path: ./part1-replica.bin
    - type: file
      name: bloc3
      path: ./part3.bin
    # - type: s3
    #   name: bloc3
    #   key: ..
    #  ..
configuration:
    logical:
        - name: dev1
          include: [bloc1, bloc2]
          capacity: "2 MiB"
          max_concurrent: 2
        - name: dev2
          include: [bloc3]
          capacity: "2 MiB"
          max_concurrent: 1
    # Final storage layout
    # [dev1: 0 - 2MB] [dev2: 2MiB - 4MiB]
    layout: [dev1, dev2]
```

All you have to do left is format the drive if not done yet then play with it.

```bash
# setup the File System if not formatted yet
brutefs format

echo "Hello World" > thething.txt
brutefs write thething.txt /test.txt

brutefs x ls . -v
# FILE 2026-05-14 16:49:49        28 B test.txt

brutefs x read test.txt | echo
# Hello World

# You can also import stored files like this..
brutefs download test.txt my_physical_copy.txt
```

# Features

- [x] File System
  - [x] brutefs core: fopen, fwrite, fseek, mkdir, fmove, fcopy, unlink,
        create_link
  - [x] native symlink support
  - [x] extra: fprepend
- [x] RAID-like configuration
  - [x] logical grouping
  - [x] replication
- [ ] No device assumption
  - [x] File device
  - [x] In memory device
  - [ ] custom KV http endpoint + Cloudflare example
  - [ ] s3 device
- [ ] Explorer
  - [x] CLI
  - [ ] WebDAV server
