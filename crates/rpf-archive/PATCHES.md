# Patches to rpf-archive 0.10.0 (Unlicense, VIRUXE/rpf-archive-rs)

Found by cross-reading with the reference library (R0 spike, 2026-10-26):
1. **Writer, RPF7 binary entries**: wrote `file_size = data length` for uncompressed files. By the convention the reference tool reads (stored size > 0 = deflate-compressed, 0 = stored as-is) that made the reference tool try to inflate raw data and fail to extract every non-resource file. Now: deflate when smaller (stored size = compressed length), else stored size 0. Nested `.rpf` entries are never compressed.
2. **Reader**: `file_size < uncompressed_size` heuristic misreads tiny files where deflate output is larger than the input (11 byte file -> 13 byte stream). Now any `file_size > 0` is inflated (falls back to raw bytes if inflate fails).

Found by the performance and robustness audit (2026-10-26):
3. **NG decryption** (`crypto/cipher.rs`, `decrypt_ng`): the 17 round keys were collected into a new heap `Vec` for every 16-byte block; they are now built once per call on the stack and blocks are decrypted in place. Same output (unit test against the original per-block code); walking every archive of the game went from about 550 to 485 ms.
4. **Directory tree** (`tree.rs`, `build_directory_tree`): folder ranges come from the file and were followed without limit, so a damaged or crafted archive whose folder lists itself (or its parent) recursed until the stack overflowed and the process aborted. Now at most one visit per entry and 128 levels (unit test with a self-listing folder).

To upstream as a PR; drop this vendored copy once released.
