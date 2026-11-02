# Third-party licenses

Read on 2026-10-07 from the pinned trees. The forks are not vendored into this repo.

## zeronsh/zui `0966d065b23e0b7c9b53c2e186705b8bce16fe2d`

A root file named `LICENSE` returns HTTP 404. The root instead has `LICENSE-APACHE` (Apache-2.0, copyright 2022–2025 Zed Industries, Inc.) and `LICENSE-GPL` (GNU GPL version 3). GitHub reports SPDX `NOASSERTION` for the repository.

Every workspace member has its own `LICENSE-APACHE`. The crates this workspace links declare `license = "Apache-2.0"`:

- `gpui`, which also has `crates/gpui/LICENSE-APACHE`
- `gpui_platform`
- `gpui_tokio`

Platform crates pulled in by `gpui_platform` (`gpui_macos`, `gpui_linux`, `gpui_windows`, `gpui_wgpu`, and the support crates) each have `LICENSE-APACHE` as well. This workspace does not contain Zed's `markdown`, `ui`, `theme`, or `editor` crates, and Keel does not depend on them.

`LICENSE-GPL` is the GPL-3 text at the root. It is not the `license` field of the crates above. The tree is not GPL-only. The binary links the Apache-2.0 crates.

## zeronsh/gpui-component `4764fd002ab5894ec9413511d01d37481b2129ab`

Root `LICENSE-APACHE` only (Apache-2.0, copyright 2024–2026 Longbridge). There is no `LICENSE-GPL`. `gpui-base` (`crates/base`) declares `license = "Apache-2.0"` and has `crates/base/LICENSE-APACHE`.

Keel depends on `gpui-base`. It does not depend on that repository's `gpui-component` UI crate.

## Lucide icons

`crates/ui/assets/icons/*.svg` are Lucide icons (https://lucide.dev), ISC License,
copied from the `gpui-component` assets tree pinned in `Cargo.toml`
(`git-branch.svg`, `square.svg`, `globe.svg`, `rotate-cw.svg` and `external-link.svg` are Lucide shapes added by hand). The ISC
notice: "Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as
part of Feather (MIT). All other copyright (c) for Lucide are held by Lucide
Contributors 2022." Permission to use, copy, modify, and/or distribute this
software for any purpose with or without fee is hereby granted, provided that the
above copyright notice and this permission notice appear in all copies.

## Harness logos

`logo-command-code.svg` is the Command Code mark, traced from commandcode.ai's `safari-pinned-tab.svg` favicon (paths unchanged, wrapper reduced to `fill="currentColor"`). It is the vendor's trademark, used only to label that harness.

## Browser pane (macOS)

The browser pane embeds the system WKWebView through [Wry](https://github.com/tauri-apps/wry) 0.56 (Apache-2.0 OR MIT), with `objc2`, `objc2-foundation`, `objc2-app-kit`, `objc2-web-kit` and `block2` (MIT). No browser engine is bundled.

The native clip view under gpui's overlay plane, and syncing its frame in `Window::on_present`, follow the approach of Zeron's `crates/ui/src/browser/macos.rs` (MIT, https://github.com/zeronsh/zeron). Keel's code is its own; no file is copied.


## Cormorant Garamond (`crates/ui/assets/fonts/hero-title.ttf`)

Cormorant Garamond by Christian Thalmann (Catharsis Fonts), SIL Open Font License 1.1. The SemiBold Italic static instance served by the Google Fonts API, fetched 2026-10-12. Used for the new-chat headline.
