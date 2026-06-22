# Changelog

## [0.4.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.3.0...v0.4.0) (2026-06-22)


### Features

* **devkitd:** add memory_limit_ticks config ([6259286](https://github.com/AbysmalBiscuit/devkit/commit/6259286bc0c90fd963a9bf32bb4deb707e0e8079))
* **devkitd:** add non-recording budget peek ([41e4881](https://github.com/AbysmalBiscuit/devkit/commit/41e4881ddaace8ebf880acc69ce9edc26b6679b2))
* **devkitd:** decide memory-limit restarts ([7370d69](https://github.com/AbysmalBiscuit/devkit/commit/7370d69bac921cab293264db20d63703cdedd78b))
* **devkitd:** restart servers over memory limit ([3e20bd6](https://github.com/AbysmalBiscuit/devkit/commit/3e20bd64355828692d80f7d79fbeb6c596341180))
* **devkitd:** serve write-decide and prefix-release over locks.sock ([1b487df](https://github.com/AbysmalBiscuit/devkit/commit/1b487df511015c62281191bf5df9ab9c2d9efb0c))
* **issue:** add multi-bar Steps progress helper ([116c347](https://github.com/AbysmalBiscuit/devkit/commit/116c347c138f90d7fd1919c50b7464a9ce7210b3))
* **issue:** add step bars and parallel history to dashboard ([ba5b3be](https://github.com/AbysmalBiscuit/devkit/commit/ba5b3bee6f19f7a6ec6dee902ab92f159d670ac5))
* **issue:** extract pr triage into devkit-issue ([27de75a](https://github.com/AbysmalBiscuit/devkit/commit/27de75ae2b2e154a348db1e7e4b4d79b078c1a8d))
* **issue:** extract status gathering into devkit-issue ([fc448a1](https://github.com/AbysmalBiscuit/devkit/commit/fc448a145cd9bb0bc032eb127bb0925e46d35460))
* **issue:** fetch prs and linear workspace in parallel ([f521289](https://github.com/AbysmalBiscuit/devkit/commit/f5212898e5c014ede87f43a25430e6359944ae39))
* **issue:** show parallel step bars for status ([cd56c98](https://github.com/AbysmalBiscuit/devkit/commit/cd56c9801b864bb90e5e98181606f26205cd15ac))
* **linear:** add per-page progress callback to issue history ([86230c4](https://github.com/AbysmalBiscuit/devkit/commit/86230c4496852e09592d035e083f82b778c3990a))
* **lockm:** add hook subcommand enforcing write access ([2bec067](https://github.com/AbysmalBiscuit/devkit/commit/2bec0671d37f600e4a460939fc8a41c1aaf406a6))
* **locks:** add ancestor-aware write decision and prefix release ([4ff8eeb](https://github.com/AbysmalBiscuit/devkit/commit/4ff8eeb75331ef19873a791a30e124030323c528))
* **locks:** add decide_write and release_prefix facade ([a93de57](https://github.com/AbysmalBiscuit/devkit/commit/a93de57609f6d94b71fcafc07f25b6ba9aca33f9))
* **locks:** add holder ancestor-or-self predicate ([817fb90](https://github.com/AbysmalBiscuit/devkit/commit/817fb90a01c22ba76d8b42dfcd4fcca017dff8c1))
* **locks:** add hook payload parsing and activation gate ([3bc4c3b](https://github.com/AbysmalBiscuit/devkit/commit/3bc4c3ba8a3c635283c46ea98e1aa8a6693a68cf))
* **locks:** add write-decide and prefix-release store ops ([0767390](https://github.com/AbysmalBiscuit/devkit/commit/0767390ae70d103ce8ff5fc7440a4951a836e42c))
* **mcp:** add devrun.down and devrun.logs actions ([24a0864](https://github.com/AbysmalBiscuit/devkit/commit/24a0864067adbb16f0344d1bd7cb0c03a5e2d499))
* **mcp:** add devrun.status action ([8f8d39c](https://github.com/AbysmalBiscuit/devkit/commit/8f8d39c3b692172e1ba2b1c7421af1ae857b75a8))
* **mcp:** add issue.status and issue.prs actions ([1ace104](https://github.com/AbysmalBiscuit/devkit/commit/1ace104c45edef29ee030e6ce9126fb64c741cc0))
* **mcp:** add non-blocking devrun.up action ([ca99e4f](https://github.com/AbysmalBiscuit/devkit/commit/ca99e4f1e789f98478ee2262fee9d7a6763bb3cc))
* **plugin:** wire write-harness hooks and dogfood opt-in ([65ec462](https://github.com/AbysmalBiscuit/devkit/commit/65ec4625d73dc93602b42cf835a7cbb353f320cd))


### Bug Fixes

* **issue:** restore prs workspace spinner step ([0aaaef8](https://github.com/AbysmalBiscuit/devkit/commit/0aaaef80e34693712d29194e1d8ec4ac9fb7133b))
* **locks:** ignore hook payloads without a session id ([dd8ebae](https://github.com/AbysmalBiscuit/devkit/commit/dd8ebae1c9058bef1a1b5e54054f83b64ee06822))
* **locks:** pin harness write locks to ttl, not pid ([89ed986](https://github.com/AbysmalBiscuit/devkit/commit/89ed9862e6e7f7b6a6832f68f5dbb4cba8cde217))
* **locks:** release holder locks across all roots ([d610b06](https://github.com/AbysmalBiscuit/devkit/commit/d610b063267a3049368791a381aa155cee1c7ec3))
* **plugin:** stop double-loading hooks/hooks.json ([16d4ba2](https://github.com/AbysmalBiscuit/devkit/commit/16d4ba287e92b1b15504aa7d1e4a15482f92cf2b))
* **registry:** detect listeners via tcp connect ([3a2ac96](https://github.com/AbysmalBiscuit/devkit/commit/3a2ac96eed0baf897ca2541da2f4d4859de07f29))


### Performance Improvements

* **issue:** fetch all prs in one graphql request ([a14b56e](https://github.com/AbysmalBiscuit/devkit/commit/a14b56ea19223e6ca5e3afba9ff7d6d510b79e46))

## [0.3.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.2.0...v0.3.0) (2026-06-22)


### Features

* add claude code plugin manifest ([efc3432](https://github.com/AbysmalBiscuit/devkit/commit/efc3432926ba5508769f05384650cacfa53e2328))
* add codex and cursor skill plugins ([8a78c0f](https://github.com/AbysmalBiscuit/devkit/commit/8a78c0f5b41cb596ad341d256a48e5b3fcd9fa2d))
* **config:** add health-probe daemon knobs ([2da382e](https://github.com/AbysmalBiscuit/devkit/commit/2da382ecad441c8b49ea283398da0e9974f70226))
* **devkitd:** restart hung servers via a health probe ([514af3b](https://github.com/AbysmalBiscuit/devkit/commit/514af3ba18e9d66c20b1870de648ed1902d33a4f))
* **devkitd:** serve the lock registry from memory over locks.sock ([c8965ab](https://github.com/AbysmalBiscuit/devkit/commit/c8965ab8ccdb2105a4ef7855c7d8bb22216f9968))
* **devkitd:** track health-probe state per supervised child ([10ac971](https://github.com/AbysmalBiscuit/devkit/commit/10ac971e638e4c4374ffc985cd44ba3d87f4d56b))
* **issue:** improve status/PR table rendering ([f1eb89c](https://github.com/AbysmalBiscuit/devkit/commit/f1eb89ceb3306e6e83e93c22338deeee342ca11d))
* **locks:** add lock daemon proto and locks.sock client ([a0e8588](https://github.com/AbysmalBiscuit/devkit/commit/a0e8588270c0fbcbf02c8573911b107fc4ded7f1))
* **locks:** add MemoryStore write-through driver and startup load ([044a036](https://github.com/AbysmalBiscuit/devkit/commit/044a0362d25b2c48df64f132b62137ea11c23507))
* **locks:** add Store seam, devkitd.lock gate, and generic *_with ops ([ae25513](https://github.com/AbysmalBiscuit/devkit/commit/ae25513f29e3223ecef27cf5a1baed8b506c1461))
* **locks:** route the facade through the daemon when one is up ([d7e25f4](https://github.com/AbysmalBiscuit/devkit/commit/d7e25f41b14687c86043f3007960afe4ac3cacc6))
* **mcp:** add action registry, describe/call, and ports actions ([4423dd3](https://github.com/AbysmalBiscuit/devkit/commit/4423dd316d1e07d8a2c39b36e21e8c5e7212ff13))
* **mcp:** add file-lock actions ([4c7f5b1](https://github.com/AbysmalBiscuit/devkit/commit/4c7f5b15bb46a9b390457bf2ce333e27d45b6e0c))
* **mcp:** handle initialize and tools/list ([c13b2f1](https://github.com/AbysmalBiscuit/devkit/commit/c13b2f128aecbfa211295be47554ee2fef2e1163))
* **mcp:** register server for Codex and Cursor ([5879a99](https://github.com/AbysmalBiscuit/devkit/commit/5879a99efe8eb4a9f82a6f273503db39fcba6db2))
* **mcp:** scaffold devkit-mcp crate with stdio json-rpc loop ([a7026be](https://github.com/AbysmalBiscuit/devkit/commit/a7026bec7aab0851bc1e06b7f2b8af5a51afa327))


### Bug Fixes

* **devkitd:** make supervisor table authoritative for restarts ([6d0b183](https://github.com/AbysmalBiscuit/devkit/commit/6d0b183882ef5d59b4610cf2e39ed3e172927acc))
* **locks:** replace stray NUL byte in test comment with its escape text ([371ac17](https://github.com/AbysmalBiscuit/devkit/commit/371ac17b61c42f2e6ee168debcbe99b030fc3d6a))
* **mcp:** register server in plugin manifest, add acquire-conflict test ([8a4f74c](https://github.com/AbysmalBiscuit/devkit/commit/8a4f74c2328b629a0b03518436961e1c21c83f42))
* **mcp:** use project root as the ports holder for liveness ([53f6620](https://github.com/AbysmalBiscuit/devkit/commit/53f66204bc0a09aac3b0c20c1007b6642b6931a0))

## [0.2.0](https://github.com/AbysmalBiscuit/devkit/compare/v0.1.0...v0.2.0) (2026-06-21)


### Features

* **common:** add slack chat.postMessage poster ([003aef1](https://github.com/AbysmalBiscuit/devkit/commit/003aef1e9a948c953b54309c53898a91055f85e2))
* **common:** git/gh subprocess wrappers with stderr-aware errors ([c3ac32c](https://github.com/AbysmalBiscuit/devkit/commit/c3ac32cfacab3a9c08b390761ef1e4a848088585))
* **common:** Linear assigned-issue history + viewer origin queries ([7566ca3](https://github.com/AbysmalBiscuit/devkit/commit/7566ca3b3ac49435b9a3af83565cb4f6e5bc8698))
* **common:** state/cache/log path helpers ([e3d937a](https://github.com/AbysmalBiscuit/devkit/commit/e3d937a9e231446b13a1ae0ab326776afdb79b22))
* **common:** table/link helpers + batched Linear Done-gate client ([a83f428](https://github.com/AbysmalBiscuit/devkit/commit/a83f428bb348927692d994e1438106689a7f49c3))
* **common:** worktree discovery + issue-id parsing ([4fd928d](https://github.com/AbysmalBiscuit/devkit/commit/4fd928d3900cfeb0dd03aadbdd8b3e6876457e3a))
* **config:** add [daemon] section with serde defaults ([fe27be9](https://github.com/AbysmalBiscuit/devkit/commit/fe27be956cf2d9a419c1c421d259c7a5e477dc35))
* **config:** add [people] aliases and defaults.pr_base ([85bb255](https://github.com/AbysmalBiscuit/devkit/commit/85bb25504fae7ecca4ed1d2e9f0bf43a364fd540))
* **config:** drive api/app conventions from config instead of hardcoding ([3cc64ac](https://github.com/AbysmalBiscuit/devkit/commit/3cc64ac515f23fed0f8817521d4d4672f6676096))
* **daemon:** client connect/handshake/autostart with flock fallback seam ([fbd3b08](https://github.com/AbysmalBiscuit/devkit/commit/fbd3b0822fb135092a95fe810a2f63ffb8ef492c))
* **daemon:** IPC protocol types and JSON-line framing ([8527cba](https://github.com/AbysmalBiscuit/devkit/commit/8527cba0ef9157ee680ef2b7d12d3f08c2a8b07a))
* **daemon:** unify transport on interprocess local sockets ([d2b72cf](https://github.com/AbysmalBiscuit/devkit/commit/d2b72cf52bd5aa40e9645ed095cfddd090a5df43))
* **devrun:** baseline worktree A/B with guarded hard-reset ([a23fe1d](https://github.com/AbysmalBiscuit/devkit/commit/a23fe1d4c9e0b8062643b09b9dd181ab82b7c3b4))
* **devrun:** detached spawn, readiness poll, SIGTERM, log tail ([9f6799b](https://github.com/AbysmalBiscuit/devkit/commit/9f6799b2cdbad7be719f18b334b90852e08d253c))
* **devrun:** doppler prefix + env layering + api-url wiring ([0c0dd39](https://github.com/AbysmalBiscuit/devkit/commit/0c0dd3943d3ca6ca5d46bb329d9f911b4dba6585))
* **devrun:** up --supervise and daemon-aware down; surface daemon supervise errors ([dac0a36](https://github.com/AbysmalBiscuit/devkit/commit/dac0a3660bb01194cf11b85cdb9657fbfde212da))
* **devrun:** up/down/status/logs with dry-run and app auto-resolution ([1f481b0](https://github.com/AbysmalBiscuit/devkit/commit/1f481b0a9ff6a66ba9fc3c25d458288256e481b1))
* example config, README, install instructions ([6a0d6b7](https://github.com/AbysmalBiscuit/devkit/commit/6a0d6b7d2d188ec729564d48d8beb948fdc97343))
* **issue-end:** Rust rewrite (gh + Linear gate + Rust cleanup) ([b5f5722](https://github.com/AbysmalBiscuit/devkit/commit/b5f5722f2efbbabad1e9be6c9e413382a1ce4174))
* **issue-prep:** mechanical worktree+env+port reservation, JSON output ([b6655ab](https://github.com/AbysmalBiscuit/devkit/commit/b6655abb51e7fe29b9decdc0eae24beffa0a5f50))
* **issue:** add review subcommand (push, PR, reviewer, slack) ([fc06961](https://github.com/AbysmalBiscuit/devkit/commit/fc06961ebe4117f48d837dc04c778d62964be738))
* **issue:** assemble dashboard issue/PR/commit timelines ([2e32992](https://github.com/AbysmalBiscuit/devkit/commit/2e32992ed8ebf6b7e7cf350ef765f0b10590c530))
* **issue:** cache the dashboard timeline fetches ([5c3c819](https://github.com/AbysmalBiscuit/devkit/commit/5c3c819ea44b892118cba8e7bce28b36c19ca798))
* **issue:** config-driven setup commands; drop .env symlink ([1a9d728](https://github.com/AbysmalBiscuit/devkit/commit/1a9d728aa1d737cb4b5deaa8ca2d33de56e92570))
* **issue:** dashboard at-a-glance view (triage + PR tables) ([9042f78](https://github.com/AbysmalBiscuit/devkit/commit/9042f7815eaabbea50343f724476697aa7ef8f0a))
* **issue:** extract shared worktree-triage core ([aced166](https://github.com/AbysmalBiscuit/devkit/commit/aced16631b6e419ad3071638ba28663be3e18ce9))
* **issue:** live dashboard data fetch (Linear/gh/git) ([724d97c](https://github.com/AbysmalBiscuit/devkit/commit/724d97c14f400c35000fa5052a11a27004429f54))
* **issue:** port issue-end clean to issue end ([c813e05](https://github.com/AbysmalBiscuit/devkit/commit/c813e055f5f1156114bc4b3ac33c7ac784386330))
* **issue:** port issue-end status to issue status ([721973f](https://github.com/AbysmalBiscuit/devkit/commit/721973fcfac39cecfbde56c1a81e3c407ffb2145))
* **issue:** port issue-prep to issue setup ([7739ae0](https://github.com/AbysmalBiscuit/devkit/commit/7739ae09282aa08c9e7d38afc638b442763b04aa))
* **issue:** port pr-status to issue prs ([a9050f0](https://github.com/AbysmalBiscuit/devkit/commit/a9050f01f2bbef9889e2424973f10f9372689548))
* **issue:** pure date bucketing and issue state replay ([bffe840](https://github.com/AbysmalBiscuit/devkit/commit/bffe8400917151a9f5ec2ec3869497da2cb33a70))
* **issue:** scaffold consolidated issue crate ([fbf084e](https://github.com/AbysmalBiscuit/devkit/commit/fbf084e95c1b1f1d3a12f0f0895231307f7ec827))
* **issue:** terminal bar and line chart rendering ([a7f4a60](https://github.com/AbysmalBiscuit/devkit/commit/a7f4a6080fb2dbd7505997bcccd803e9ee8ba4e8))
* **locks:** acquire/release/check/prune operations ([e33bd9d](https://github.com/AbysmalBiscuit/devkit/commit/e33bd9d576809c670262c7394abc72665a135985))
* **locks:** flock-guarded JSON lock store with salvage ([7643b6c](https://github.com/AbysmalBiscuit/devkit/commit/7643b6ce81e90c84e176e11e83bf78dce80dec1d))
* **locks:** lock CLI binary and startup state migration ([d45d723](https://github.com/AbysmalBiscuit/devkit/commit/d45d72319249a12036647f4fcb8810193adb867b))
* **locks:** lock entry model and path-overlap detection ([c68ab96](https://github.com/AbysmalBiscuit/devkit/commit/c68ab96f2607338b7247cc6571a968be2e1d19b6))
* **locks:** root detection, path normalization, and public ops ([9319a0d](https://github.com/AbysmalBiscuit/devkit/commit/9319a0d854d9ed7e23f0cd93306f8d1b6697e462))
* **locks:** scaffold devkit-locks crate ([35c6d16](https://github.com/AbysmalBiscuit/devkit/commit/35c6d16a11256cfd2a92abe0f628642848e7ead1))
* **locks:** session identity precedence and anchor-pid policy ([47be011](https://github.com/AbysmalBiscuit/devkit/commit/47be01127857565b01bbdb86b9d9705b3d7684f2))
* native Windows build for paths, devrun logs, and tests ([830f4cb](https://github.com/AbysmalBiscuit/devkit/commit/830f4cb37c82405d97cfef7a2034b866a8528d20))
* one-command install via `cargo install --path .` + shell completions ([a311b3e](https://github.com/AbysmalBiscuit/devkit/commit/a311b3e65f5e93b362570ca46c9ac84caabc6c20))
* **paths:** add daemon socket/lock/log paths ([66b3567](https://github.com/AbysmalBiscuit/devkit/commit/66b35673d54b9f9e157fe94f0e5bbf0247b212b2))
* **paths:** move state home to XDG ~/.local/state/devkit with legacy fallback ([1b19e5f](https://github.com/AbysmalBiscuit/devkit/commit/1b19e5f3b274ef9eae4982bc683a776a69146770))
* **portd:** daemon skeleton — single-instance lock, socket, idle-exit ([a6ad9d0](https://github.com/AbysmalBiscuit/devkit/commit/a6ad9d08939723556155200bb42e2ec7720e53b8))
* **portd:** request dispatch, supervision thread, restart, adoption, down coordination ([cecb3e4](https://github.com/AbysmalBiscuit/devkit/commit/cecb3e4184f424f27bd0ab77cf1ad5e2d936fd17))
* **portd:** serve the port registry from authoritative memory ([a43b926](https://github.com/AbysmalBiscuit/devkit/commit/a43b92630626a7ddb7a16eb8ea1071375f927e08))
* **portd:** supervisor table — reap, crash-loop budget, memory tracking, adoption ([f3b02ed](https://github.com/AbysmalBiscuit/devkit/commit/f3b02edfc0f5fae96300975e82df7f4ff8d9eb06))
* **portman:** status/release/prune CLI over the registry ([f9f67f6](https://github.com/AbysmalBiscuit/devkit/commit/f9f67f67635bfa2ab786528e10857312cb4f12db))
* **ports:** app catalog merging config with doppler.yaml ([b6427d7](https://github.com/AbysmalBiscuit/devkit/commit/b6427d7d0f31e3101b7f786afb362e426adcc0a5))
* **ports:** devkit.toml config + doppler.yaml parsing (prd denylist) ([7879893](https://github.com/AbysmalBiscuit/devkit/commit/7879893877fecca61bf3156e4e19d11f723d182e))
* **ports:** registry alloc/release/prune (idempotent reservations) ([8ee1b7e](https://github.com/AbysmalBiscuit/devkit/commit/8ee1b7ee9b97c5f2ca288800b055078e5bf9a834))
* **ports:** registry liveness helpers (listening/pid/holder) ([db02eac](https://github.com/AbysmalBiscuit/devkit/commit/db02eac75db9910d136fcc2237dc647fe37632c4))
* **ports:** registry types, RAII flock, atomic load/save ([e867ddd](https://github.com/AbysmalBiscuit/devkit/commit/e867ddd3d6b7f89c5e4225747a1940826813dc1c))
* **ports:** shared config→catalog loader; wire portman alloc ([8d839ed](https://github.com/AbysmalBiscuit/devkit/commit/8d839ed29459c825e68db9d8ac9c331e4e822a5e))
* **pr-status:** Rust rewrite with before→after diff cache ([ee6926b](https://github.com/AbysmalBiscuit/devkit/commit/ee6926bec384f48d0469b6e29c66ae25059a6b12))
* **registry:** MemoryStore driver with write-through commit point ([f1a6bdf](https://github.com/AbysmalBiscuit/devkit/commit/f1a6bdf059c3b91d5a4b2534d833393073c6cf0a))
* **registry:** route facade through daemon when up, flock fallback ([0e42918](https://github.com/AbysmalBiscuit/devkit/commit/0e42918303e23bd4fdd93148d94e0b52b4237c5f))
* **store:** expose load/save for lock-free owners ([f0c9fb2](https://github.com/AbysmalBiscuit/devkit/commit/f0c9fb2a52615a3d056138239b684be5daae7532))
* **sys:** add Windows backend via windows-sys ([163a948](https://github.com/AbysmalBiscuit/devkit/commit/163a948e9882d0bc5dff04d6315edc57f244ce69))
* **sys:** parent-pid and controlling-tty behind the boundary ([fa7fccf](https://github.com/AbysmalBiscuit/devkit/commit/fa7fccfd6a1a3d0a6f2b0a7211eaa428ac44fcbe))


### Bug Fixes

* **issue:** harden review URL parsing, Linear pagination, and dashboard repo discovery ([7f90ada](https://github.com/AbysmalBiscuit/devkit/commit/7f90adaaf035b63693fada810495e53050211043))
* **issue:** keep dashboard rendering when the PR panel fails ([51bd206](https://github.com/AbysmalBiscuit/devkit/commit/51bd20670088a2c42a1f1b3bc6c7b0f224ed0af7))
* **portd:** load the registry before binding the socket ([47d5bce](https://github.com/AbysmalBiscuit/devkit/commit/47d5bce3d8d3c3e95a080315e9f9010e7e844233))
* **ports:** record_pid upserts + down stops un-pruned entries; grace &gt; readiness timeout ([be0ceb1](https://github.com/AbysmalBiscuit/devkit/commit/be0ceb17e9f6951fb58f9d84ec8c66e60038a5a8))
* **ports:** skip apps with unresolvable path instead of failing the catalog ([6949db4](https://github.com/AbysmalBiscuit/devkit/commit/6949db4a2065f95c0c31b4898f5a4265a86caef1))
* **registry:** make the portd.lock gate unconditional and leak-proof ([6319432](https://github.com/AbysmalBiscuit/devkit/commit/6319432e3442cd4358bae4f1dd9a834507145e05))
* **release:** give root package a concrete version for release-please ([e9913e6](https://github.com/AbysmalBiscuit/devkit/commit/e9913e69c7ea824d62d91429f6530d5afee38c7d))
* **release:** pin member crate versions for release-please ([8415ebb](https://github.com/AbysmalBiscuit/devkit/commit/8415ebb65163630ad827619d7361b67d190ec7ee))
* **sys:** compute tree_rss_bytes on macOS via ps ([6f1bec8](https://github.com/AbysmalBiscuit/devkit/commit/6f1bec8b03eb50302fefa9ba6e37b7e2adb3df14))


### Performance Improvements

* add release profile (thin LTO, codegen-units=1, panic=abort, strip) ([ec0f66f](https://github.com/AbysmalBiscuit/devkit/commit/ec0f66fd3f98c65167d689744de6d91c1bc38edb))
* **pr-status:** parallelize independent gh round-trips ([174dcf6](https://github.com/AbysmalBiscuit/devkit/commit/174dcf6337b23a5e2e0416a761b35b6a5b281b4b))
