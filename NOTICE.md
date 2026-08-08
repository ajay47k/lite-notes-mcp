# Third-Party Notices

lite-notes is MIT licensed (see [LICENSE](LICENSE)). It bundles the following
third-party components, vendored under `src/vendor/` so the app runs fully
offline with no CDN requests at runtime.

| Component | Version | License | Project |
|---|---|---|---|
| marked | 15.x | MIT | https://github.com/markedjs/marked |
| DOMPurify | 3.4.13 | Apache-2.0 OR MPL-2.0 | https://github.com/cure53/DOMPurify |
| highlight.js | 11.x | BSD-3-Clause | https://github.com/highlightjs/highlight.js |
| github-markdown-css | 5.x | MIT | https://github.com/sindresorhus/github-markdown-css |

Each vendored file retains its original license banner. No source modifications
were made to any of them.

The application is built with [Tauri](https://tauri.app) v2 (MIT OR Apache-2.0).
Rust crate dependencies and their licenses are recorded in
`src-tauri/Cargo.lock`.
