
### Image decoder dependencies

The exact-pinned `image 0.25.9` dependency enables only PNG, JPEG, GIF and WebP.
The following additions to Cargo.lock have license texts retained under `licenses/`;
release packages must carry this directory with this notice register.

| Dependency | License | License directory |
| --- | --- | --- |
| `adler2 2.0.1` | 0BSD OR MIT OR Apache-2.0 | `licenses/adler2-2.0.1` |
| `bytemuck 1.25.2` | Zlib OR Apache-2.0 OR MIT | `licenses/bytemuck-1.25.2` |
| `byteorder-lite 0.1.0` | Unlicense OR MIT | `licenses/byteorder-lite-0.1.0` |
| `color_quant 1.1.0` | MIT | `licenses/color_quant-1.1.0` |
| `crc32fast 1.5.2` | MIT OR Apache-2.0 | `licenses/crc32fast-1.5.2` |
| `fdeflate 0.3.7` | MIT OR Apache-2.0 | `licenses/fdeflate-0.3.7` |
| `flate2 1.1.10` | MIT OR Apache-2.0 | `licenses/flate2-1.1.10` |
| `gif 0.14.2` | MIT OR Apache-2.0 | `licenses/gif-0.14.2` |
| `image 0.25.9` | MIT OR Apache-2.0 | `licenses/image-0.25.9` |
| `image-webp 0.2.4` | MIT OR Apache-2.0 | `licenses/image-webp-0.2.4` |
| `miniz_oxide 0.8.9` | MIT OR Zlib OR Apache-2.0 | `licenses/miniz_oxide-0.8.9` |
| `miniz_oxide 0.9.1` | MIT OR Zlib OR Apache-2.0 | `licenses/miniz_oxide-0.9.1` |
| `moxcms 0.7.11` | BSD-3-Clause OR Apache-2.0 | `licenses/moxcms-0.7.11` |
| `png 0.18.1` | MIT OR Apache-2.0 | `licenses/png-0.18.1` |
| `pxfm 0.1.30` | BSD-3-Clause OR Apache-2.0 | `licenses/pxfm-0.1.30` |
| `quick-error 2.0.1` | MIT/Apache-2.0 | `licenses/quick-error-2.0.1` |
| `simd-adler32 0.3.10` | MIT | `licenses/simd-adler32-0.3.10` |
| `weezl 0.1.12` | MIT OR Apache-2.0 | `licenses/weezl-0.1.12` |
| `zlib-rs 0.6.8` | Zlib | `licenses/zlib-rs-0.6.8` |
| `zune-core 0.5.3` | MIT OR Apache-2.0 OR Zlib | `licenses/zune-core-0.5.3` |
| `zune-jpeg 0.5.15` | MIT OR Apache-2.0 OR Zlib | `licenses/zune-jpeg-0.5.15` |


This is not a retrospective license audit of existing Grapt source or dependencies.

The web adapter depends on [htmd 0.5.4](https://github.com/letmutex/htmd),
licensed Apache-2.0. Its published crate license was inspected before adoption;
the Apache text below applies. Release dependency inventories must also include
its html5ever, markup5ever_rcdom, and phf dependencies and their applicable notices.

