# Dev mode: `--dev`

```bash
# Not persistent accross runs
xhfs upload test.jpg --dev

# Will allocate 134217728  * 2 * 3 bytes sized in-memory buffer
xhfs server webdav \
    --dev \
    --dev-unit-capacity 134217728 \
    --dev-replica-count 2 \
    --dev-logical-count 3
```
