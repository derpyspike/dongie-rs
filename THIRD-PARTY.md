# Third-party components

dongie is licensed under the EUPL-1.2 (see `LICENSE`). The release binary statically links the
Rust crates below (resolved for `aarch64-unknown-linux-musl`). All of them are under permissive licenses
(MIT, Apache-2.0, BSD-3-Clause, BSL-1.0, Unlicense, Unicode-3.0) that allow this combination as long as their
copyright and license notices are kept; the full texts ship with each crate's source on crates.io.

The web UI uses the colour values of the [Catppuccin](https://github.com/catppuccin/catppuccin) palette (MIT).
The binary also contains the Rust standard library (MIT OR Apache-2.0).

Regenerate the table with `cargo metadata --format-version 1 --filter-platform aarch64-unknown-linux-musl`.

| Crate | Version | License | Source |
|---|---|---|---|
| argon2 | 0.5.3 | MIT OR Apache-2.0 | https://github.com/RustCrypto/password-hashes/tree/master/argon2 |
| async-broadcast | 0.7.2 | MIT OR Apache-2.0 | https://github.com/smol-rs/async-broadcast |
| async-recursion | 1.2.0 | MIT OR Apache-2.0 | https://github.com/dcchut/async-recursion |
| async-trait | 0.1.92 | MIT OR Apache-2.0 | https://github.com/dtolnay/async-trait |
| atomic-waker | 1.1.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/atomic-waker |
| axum | 0.8.9 | MIT | https://github.com/tokio-rs/axum |
| axum-core | 0.5.6 | MIT | https://github.com/tokio-rs/axum |
| base64ct | 1.8.3 | Apache-2.0 OR MIT | https://github.com/RustCrypto/formats |
| bitflags | 2.13.2 | MIT OR Apache-2.0 | https://github.com/bitflags/bitflags |
| blake2 | 0.10.6 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | https://github.com/RustCrypto/utils |
| bytes | 1.12.2 | MIT | https://github.com/tokio-rs/bytes |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/cfg-if |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| digest | 0.10.7 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits |
| endi | 1.1.1 | MIT | https://github.com/zeenix/endi |
| enumflags2 | 0.7.12 | MIT OR Apache-2.0 | https://github.com/meithecatte/enumflags2 |
| enumflags2_derive | 0.7.12 | MIT OR Apache-2.0 | https://github.com/meithecatte/enumflags2 |
| equivalent | 1.0.3 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/equivalent |
| errno | 0.3.15 | MIT OR Apache-2.0 | https://github.com/lambda-fairy/rust-errno |
| event-listener | 5.4.2 | Apache-2.0 OR MIT | https://github.com/smol-rs/event-listener |
| event-listener-strategy | 0.5.4 | Apache-2.0 OR MIT | https://github.com/smol-rs/event-listener-strategy |
| fastrand | 2.5.0 | Apache-2.0 OR MIT | https://github.com/smol-rs/fastrand |
| form_urlencoded | 1.2.3 | MIT OR Apache-2.0 | https://github.com/servo/rust-url |
| futures-core | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-io | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-lite | 2.6.1 | Apache-2.0 OR MIT | https://github.com/smol-rs/futures-lite |
| futures-task | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| futures-util | 0.3.34 | MIT OR Apache-2.0 | https://github.com/rust-lang/futures-rs |
| generic-array | 0.14.7 | MIT | https://github.com/fizyk20/generic-array.git |
| getrandom | 0.2.17 | MIT OR Apache-2.0 | https://github.com/rust-random/getrandom |
| hashbrown | 0.17.2 | MIT OR Apache-2.0 | https://github.com/rust-lang/hashbrown |
| hex | 0.4.3 | MIT OR Apache-2.0 | https://github.com/KokaKiwi/rust-hex |
| http | 1.5.0 | MIT OR Apache-2.0 | https://github.com/hyperium/http |
| http-body | 1.1.0 | MIT | https://github.com/hyperium/http-body |
| http-body-util | 0.1.5 | MIT | https://github.com/hyperium/http-body |
| httparse | 1.10.1 | MIT OR Apache-2.0 | https://github.com/seanmonstar/httparse |
| httpdate | 1.0.3 | MIT OR Apache-2.0 | https://github.com/pyfisch/httpdate |
| hyper | 1.13.0 | MIT | https://github.com/hyperium/hyper |
| hyper-util | 0.1.21 | MIT | https://github.com/hyperium/hyper-util |
| indexmap | 2.14.2 | Apache-2.0 OR MIT | https://github.com/indexmap-rs/indexmap |
| itoa | 1.0.19 | MIT OR Apache-2.0 | https://github.com/dtolnay/itoa |
| libc | 0.2.191 | MIT OR Apache-2.0 | https://github.com/rust-lang/libc |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/sunfishcode/linux-raw-sys |
| matchit | 0.8.4 | MIT AND BSD-3-Clause | https://github.com/ibraheemdev/matchit |
| memchr | 2.8.3 | Unlicense OR MIT | https://github.com/BurntSushi/memchr |
| mime | 0.3.18 | MIT OR Apache-2.0 | https://github.com/hyperium/mime |
| mio | 1.2.5 | MIT | https://github.com/tokio-rs/mio |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | https://github.com/matklad/once_cell |
| ordered-stream | 0.2.0 | MIT OR Apache-2.0 | https://github.com/danieldg/ordered-stream |
| parking | 2.2.1 | Apache-2.0 OR MIT | https://github.com/smol-rs/parking |
| password-hash | 0.5.0 | MIT OR Apache-2.0 | https://github.com/RustCrypto/traits/tree/master/password-hash |
| percent-encoding | 2.3.2 | MIT OR Apache-2.0 | https://github.com/servo/rust-url/ |
| pin-project-lite | 0.2.18 | Apache-2.0 OR MIT | https://github.com/taiki-e/pin-project-lite |
| proc-macro-crate | 3.5.0 | MIT OR Apache-2.0 | https://github.com/bkchr/proc-macro-crate |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 | https://github.com/dtolnay/proc-macro2 |
| quote | 1.0.47 | MIT OR Apache-2.0 | https://github.com/dtolnay/quote |
| rand_core | 0.6.4 | MIT OR Apache-2.0 | https://github.com/rust-random/rand |
| rpassword | 7.5.5 | Apache-2.0 | https://github.com/conradkleinespel/rpassword |
| rtoolbox | 0.0.6 | Apache-2.0 | https://github.com/conradkleinespel/rtoolbox |
| rustix | 1.1.6 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | https://github.com/bytecodealliance/rustix |
| ryu | 1.0.23 | Apache-2.0 OR BSL-1.0 | https://github.com/dtolnay/ryu |
| serde | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_core | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | https://github.com/serde-rs/json |
| serde_path_to_error | 0.1.20 | MIT OR Apache-2.0 | https://github.com/dtolnay/path-to-error |
| serde_repr | 0.1.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/serde-repr |
| serde_spanned | 0.6.9 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| serde_urlencoded | 0.7.1 | MIT/Apache-2.0 | https://github.com/nox/serde_urlencoded |
| signal-hook-registry | 1.4.8 | MIT OR Apache-2.0 | https://github.com/vorner/signal-hook |
| slab | 0.4.12 | MIT | https://github.com/tokio-rs/slab |
| smallvec | 1.16.6 | MIT OR Apache-2.0 | https://github.com/smallvec/smallvec |
| socket2 | 0.6.5 | MIT OR Apache-2.0 | https://github.com/rust-lang/socket2 |
| subtle | 2.6.1 | BSD-3-Clause | https://github.com/dalek-cryptography/subtle |
| syn | 2.0.119 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| syn | 3.0.10 | MIT OR Apache-2.0 | https://github.com/dtolnay/syn |
| sync_wrapper | 1.0.2 | Apache-2.0 | https://github.com/Actyx/sync_wrapper |
| tokio | 1.54.1 | MIT | https://github.com/tokio-rs/tokio |
| tokio-macros | 2.7.2 | MIT | https://github.com/tokio-rs/tokio |
| toml | 0.8.23 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 0.6.11 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_datetime | 1.1.3+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.22.27 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_edit | 0.25.20+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| toml_parser | 1.1.6+spec-1.1.0 | MIT OR Apache-2.0 | https://github.com/toml-rs/toml |
| tower | 0.5.3 | MIT | https://github.com/tower-rs/tower |
| tower-layer | 0.3.3 | MIT | https://github.com/tower-rs/tower |
| tower-service | 0.3.3 | MIT | https://github.com/tower-rs/tower |
| tracing | 0.1.44 | MIT | https://github.com/tokio-rs/tracing |
| tracing-attributes | 0.1.31 | MIT | https://github.com/tokio-rs/tracing |
| tracing-core | 0.1.36 | MIT | https://github.com/tokio-rs/tracing |
| typenum | 1.20.1 | MIT OR Apache-2.0 | https://github.com/paholg/typenum |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 | https://github.com/dtolnay/unicode-ident |
| uuid | 1.29.3 | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid |
| version_check | 0.9.5 | MIT/Apache-2.0 | https://github.com/SergioBenitez/version_check |
| winnow | 0.7.15 | MIT | https://github.com/winnow-rs/winnow |
| winnow | 1.0.5 | MIT | https://github.com/winnow-rs/winnow |
| zbus | 5.19.0 | MIT | https://github.com/z-galaxy/zbus/ |
| zbus_macros | 5.19.0 | MIT | https://github.com/z-galaxy/zbus/ |
| zbus_names | 4.3.5 | MIT | https://github.com/z-galaxy/zbus/ |
| zcheapstr | 1.4.0 | MIT | https://github.com/z-galaxy/zcheapstr/ |
| zmij | 1.0.23 | MIT | https://github.com/dtolnay/zmij |
| zvariant | 5.15.1 | MIT | https://github.com/z-galaxy/zbus/ |
| zvariant_derive | 5.15.1 | MIT | https://github.com/z-galaxy/zbus/ |
| zvariant_utils | 4.2.0 | MIT | https://github.com/z-galaxy/zbus/ |
