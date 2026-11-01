# Slint 1.18.1 Windows software redraw backport

The QDM release build uses a project-local copy of **i-slint-backend-winit 1.18.1** in
`vendor/i-slint-backend-winit-1.18.1/`. Cargo's `[patch.crates-io]` entry in
`Cargo.toml` selects this copy; no global Cargo cache is edited.

## Why

The original 1.18.1 software renderer can leave portions of a Windows window
unpainted after minimizing/restoring or a DPI/monitor change. Slint fixed the
invalidated-region tracking after 1.18.1 in PR
[slint-ui/slint#13568](https://github.com/slint-ui/slint/pull/13568),
merged as `08a297b` on 2026-09-28.

## Change

Only `winitwindowadapter.rs` differs in runtime behavior: call
`mark_windows_update_region_dirty()` before rendering on Windows, using the
Win32 update rectangle to mark the area dirty in Slint's core renderer.
It avoids full-window invalidation in normal frames and preserves the
low-memory software backend.

This is a targeted backport, **not** an upgrade of Slint or a claim to fix
every possible Windows compositor artifact. The original crate license and
copyright notices remain in the vendored source. Review the source license
before redistributing the binary.

## Verification

Build with `scripts/build.ps1` (`cargo build --release --locked`).
Test minimize/restore, resize, first launch, and switching monitors/DPI when
available. Inspect the actual on-screen window, not PrintWindow alone.
Check that idle CPU and private RAM have not regressed.
